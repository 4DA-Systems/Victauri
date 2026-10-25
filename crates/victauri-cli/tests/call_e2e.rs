//! End-to-end tests of `victauri tools` / `victauri call`: the real binary against a mock
//! Victauri server found through REAL discovery files in a private temp root. The mock
//! enforces the discovery token, so every passing call also proves the CLI authenticated the
//! way `invoke` does — and the tests check that token never reaches the terminal.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use serde_json::{Value, json};

const TOKEN: &str = "call-e2e-secret-token-7f3a";
/// The bytes the mock's `screenshot` returns (a PNG signature + payload).
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4];

struct IsolatedTemp(tempfile::TempDir);

impl IsolatedTemp {
    fn new() -> Self {
        Self(tempfile::tempdir().expect("create isolated temp dir"))
    }

    fn write_entry(&self, port: u16, identifier: &str) {
        let pid = std::process::id();
        let dir = self.0.path().join("victauri").join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for d in [self.0.path().join("victauri"), dir.clone()] {
                std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        std::fs::write(dir.join("port"), port.to_string()).unwrap();
        std::fs::write(dir.join("token"), TOKEN).unwrap();
        std::fs::write(
            dir.join("metadata.json"),
            json!({"pid": pid, "port": port, "identifier": identifier, "product_name": "Mock"})
                .to_string(),
        )
        .unwrap();
    }

    fn victauri(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_victauri"));
        for var in ["TMP", "TEMP", "TMPDIR", "XDG_RUNTIME_DIR"] {
            cmd.env(var, self.0.path());
        }
        for var in ["VICTAURI_PORT", "VICTAURI_AUTH_TOKEN", "VICTAURI_APP"] {
            cmd.env_remove(var);
        }
        // Never discover through the home state root of the machine running the tests.
        cmd.env("HOME", self.0.path()).env_remove("XDG_STATE_HOME");
        cmd.args(args);
        cmd
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }
}

#[derive(Clone)]
struct Mock {
    identifier: String,
    plugin_version: String,
}

fn text(value: &Value) -> Value {
    json!({"content": [{"type": "text", "text": value.to_string()}]})
}

fn tool_schema(name: &str) -> Value {
    match name {
        "query_db" => json!({
            "name": "query_db",
            "description": "Run a read-only SQL query. Enforced by an authorizer.",
            "inputSchema": {
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {"type": "string", "description": "The SQL. One statement."},
                    "max_rows": {"type": ["integer", "null"], "description": "Row cap."}
                }
            }
        }),
        _ => json!({"name": name, "description": format!("{name} tool."),
                    "inputSchema": {"type": "object"}}),
    }
}

async fn mcp(State(mock): State<Mock>, headers: HeaderMap, body: String) -> Response {
    // Like the plugin: auth on, the discovery token is the only key.
    let auth = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if auth != format!("Bearer {TOKEN}") {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    let v: Value = serde_json::from_str(&body).unwrap_or_else(|_| json!({}));
    let id = v.get("id").cloned();
    let reply = |result: Value| Json(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    match v["method"].as_str().unwrap_or("") {
        "initialize" => reply(json!({"protocolVersion": "2025-03-26", "capabilities": {},
                                     "serverInfo": {"name": "mock", "version": "0"}}))
        .into_response(),
        "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
        "tools/list" => reply(json!({"tools": [
            tool_schema("query_db"), tool_schema("screenshot"), tool_schema("echo")
        ]}))
        .into_response(),
        "tools/call" => {
            let name = v["params"]["name"].as_str().unwrap_or("");
            let arguments = v["params"]["arguments"].clone();
            let result = match name {
                "get_plugin_info" => text(&json!({
                    "version": mock.plugin_version,
                    "app": {"identifier": mock.identifier},
                })),
                "echo" => text(&json!({"received": arguments})),
                "screenshot" => json!({"content": [{
                    "type": "image",
                    "data": base64::engine::general_purpose::STANDARD.encode(PNG),
                    "mimeType": "image/png"
                }]}),
                "trace" => json!({"content": [
                    {"type": "text", "text": "{\"frames\":2}"},
                    {"type": "image", "data": base64::engine::general_purpose::STANDARD.encode(PNG),
                     "mimeType": "image/png"},
                    {"type": "image", "data": base64::engine::general_purpose::STANDARD.encode(PNG),
                     "mimeType": "image/png"}
                ]}),
                // A page-controlled error text that tries to forge a CI annotation and drive
                // the terminal.
                "fail" => json!({"isError": true, "content": [{"type": "text",
                    "text": "boom\n::error file=x::forged\u{1b}[31m red"}]}),
                _ => {
                    return Json(json!({"jsonrpc": "2.0", "id": id, "error": {
                        "code": -32602, "message": format!("tool not found: {name}")
                    }}))
                    .into_response();
                }
            };
            reply(result).into_response()
        }
        _ => reply(json!({})).into_response(),
    }
}

async fn start_mock(identifier: &str, plugin_version: &str) -> u16 {
    let state = Mock {
        identifier: identifier.to_string(),
        plugin_version: plugin_version.to_string(),
    };
    let info = json!({"app_identifier": identifier});
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/info", get(move || async move { Json(info) }))
        .route("/mcp", post(mcp))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    port
}

async fn run(cmd: Command) -> Output {
    run_with_stdin(cmd, None).await
}

async fn run_with_stdin(mut cmd: Command, stdin: Option<&'static str>) -> Output {
    tokio::task::spawn_blocking(move || {
        cmd.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("run victauri");
        if let Some(input) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn json_stdout(out: &Output) -> Value {
    serde_json::from_str(&stdout(out))
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{}", stdout(out)))
}

/// The discovery token must never be printed, whatever the outcome.
fn assert_no_token(out: &Output) {
    assert!(
        !stdout(out).contains(TOKEN) && !stderr(out).contains(TOKEN),
        "the auth token reached the terminal:\n{}\n{}",
        stdout(out),
        stderr(out)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_maps_shell_arguments_to_tool_json() {
    let iso = IsolatedTemp::new();
    iso.write_entry(
        start_mock("com.test.call", env!("CARGO_PKG_VERSION")).await,
        "com.test.call",
    );

    let out = run(iso.victauri(&[
        "call",
        "echo",
        r#"{"base":"json","n":1}"#,
        "n:=2",
        "query=SELECT * FROM t WHERE a = 'x'",
        "deep:={\"list\":[1,true,null]}",
        "id=007",
    ]))
    .await;
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        json_stdout(&out)["received"],
        json!({"base": "json", "n": 2, "query": "SELECT * FROM t WHERE a = 'x'",
               "deep": {"list": [1, true, null]}, "id": "007"})
    );
    assert_no_token(&out);

    // `--raw` is one line of JSON.
    let out = run(iso.victauri(&["call", "echo", "a=b", "--raw"])).await;
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim_end().lines().count(),
        1,
        "{}",
        stdout(&out)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_reads_arguments_from_stdin() {
    let iso = IsolatedTemp::new();
    iso.write_entry(
        start_mock("com.test.stdin", env!("CARGO_PKG_VERSION")).await,
        "com.test.stdin",
    );

    let out = run_with_stdin(
        iso.victauri(&["call", "echo", "--args-file", "-", "extra=1"]),
        Some(r#"{"action":"list","limit":3}"#),
    )
    .await;
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        json_stdout(&out)["received"],
        json!({"action": "list", "limit": 3, "extra": "1"})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_saves_images_with_out_and_never_prints_base64() {
    let iso = IsolatedTemp::new();
    iso.write_entry(
        start_mock("com.test.img", env!("CARGO_PKG_VERSION")).await,
        "com.test.img",
    );
    let b64 = base64::engine::general_purpose::STANDARD.encode(PNG);

    let shot = iso.path("shot.png");
    let out = run(iso.victauri(&["call", "screenshot", "--out", shot.to_str().unwrap()])).await;
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        std::fs::read(&shot).unwrap(),
        PNG,
        "the decoded PNG is written"
    );
    let v = json_stdout(&out);
    assert_eq!(v["images"][0]["bytes"], PNG.len());
    assert_eq!(v["images"][0]["mimeType"], "image/png");
    assert!(
        v["images"][0]["saved"]
            .as_str()
            .unwrap()
            .ends_with("shot.png")
    );
    assert!(!stdout(&out).contains(&b64));

    // Several images are numbered beside --out; the text block survives.
    let frames = iso.path("frame.png");
    let out = run(iso.victauri(&["call", "trace", "--out", frames.to_str().unwrap()])).await;
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(std::fs::read(iso.path("frame-1.png")).unwrap(), PNG);
    assert_eq!(std::fs::read(iso.path("frame-2.png")).unwrap(), PNG);
    assert!(!frames.exists());
    assert_eq!(json_stdout(&out)["text"], json!({"frames": 2}));

    // Without --out the image is summarized, not dumped.
    let out = run(iso.victauri(&["call", "screenshot"])).await;
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stdout(&out).contains(&b64));
    assert!(stderr(&out).contains("--out"), "{}", stderr(&out));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_error_exits_1_with_json_and_a_sanitized_message() {
    let iso = IsolatedTemp::new();
    iso.write_entry(
        start_mock("com.test.err", env!("CARGO_PKG_VERSION")).await,
        "com.test.err",
    );

    let out = run(iso.victauri(&["call", "fail"])).await;
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let v = json_stdout(&out);
    assert!(v["error"].as_str().unwrap().starts_with("boom"), "{v}");
    let err = stderr(&out);
    assert!(
        !err.lines().any(|l| l.trim_start().starts_with("::")),
        "no forgeable CI annotation line:\n{err}"
    );
    assert!(!err.contains('\u{1b}'), "no raw escape sequences:\n{err}");
    // Same plugin version → no skew warning.
    assert!(!err.contains("Version skew"), "{err}");
    assert_no_token(&out);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_tool_exits_1_and_points_at_invoke_command() {
    let iso = IsolatedTemp::new();
    iso.write_entry(
        start_mock("com.test.unknown", env!("CARGO_PKG_VERSION")).await,
        "com.test.unknown",
    );

    let out = run(iso.victauri(&["call", "get_settings"])).await;
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        json_stdout(&out)["error"]
            .as_str()
            .unwrap()
            .contains("tool not found")
    );
    let err = stderr(&out);
    assert!(
        err.contains("victauri call invoke_command command=get_settings"),
        "an app command typed as a tool gets the right route:\n{err}"
    );

    let out = run(iso.victauri(&["call", "query"])).await;
    assert!(
        stderr(&out).contains("did you mean: query_db"),
        "{}",
        stderr(&out)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failure_against_a_skewed_plugin_warns() {
    let iso = IsolatedTemp::new();
    iso.write_entry(start_mock("com.test.skew", "0.1.0").await, "com.test.skew");

    let out = run(iso.victauri(&["call", "fail"])).await;
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("Version skew"), "{}", stderr(&out));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_arguments_exit_2_before_any_connection() {
    let iso = IsolatedTemp::new();
    // No discovery entry at all: an argument error must win over "no app".
    let out = run(iso.victauri(&["call", "echo", "not-a-pair"])).await;
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("key=value"), "{}", stderr(&out));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_running_app_exits_2_for_call_and_tools_falls_back_to_the_builtin_list() {
    let iso = IsolatedTemp::new();

    let out = run(iso.victauri(&["call", "dom_snapshot"])).await;
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));

    let out = run(iso.victauri(&["tools", "--json"])).await;
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json_stdout(&out);
    assert_eq!(v["source"], "builtin");
    assert_eq!(v["tools"].as_array().unwrap().len(), 35);
    assert!(stderr(&out).contains("no Victauri app reachable"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_lists_live_schemas_and_describes_one() {
    let iso = IsolatedTemp::new();
    iso.write_entry(
        start_mock("com.test.tools", env!("CARGO_PKG_VERSION")).await,
        "com.test.tools",
    );

    let out = run(iso.victauri(&["tools", "--json"])).await;
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json_stdout(&out);
    assert_eq!(v["source"], "live");
    assert_eq!(v["app"], "com.test.tools");
    assert_eq!(v["tools"][0]["inputSchema"]["required"], json!(["query"]));

    let out = run(iso.victauri(&["tools"])).await;
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.contains("3 tools on com.test.tools"), "{text}");
    assert!(text.contains("query_db") && text.contains("Run a read-only SQL query."));

    let out = run(iso.victauri(&["tools", "query_db"])).await;
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(
        text.contains("query") && text.contains("[required]"),
        "{text}"
    );
    assert!(
        text.contains("max_rows") && text.contains("integer"),
        "{text}"
    );

    let out = run(iso.victauri(&["tools", "quer"])).await;
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("did you mean: query_db"),
        "{}",
        stderr(&out)
    );
    assert_no_token(&out);
}
