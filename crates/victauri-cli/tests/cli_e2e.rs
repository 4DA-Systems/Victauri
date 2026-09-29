//! End-to-end tests of the real `victauri` CLI (check / invoke) against mock Victauri
//! servers found through REAL discovery files in a private temp root — never the machine's
//! own discovery directory, and never a real app's port.

use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

/// A private temp root; the CLI under test resolves its discovery dir from it.
struct IsolatedTemp(tempfile::TempDir);

impl IsolatedTemp {
    fn new() -> Self {
        Self(tempfile::tempdir().expect("create isolated temp dir"))
    }

    /// Write a discovery entry exactly as the plugin does.
    fn write_entry(&self, pid: u32, port: u16, token: &str, identifier: &str) -> PathBuf {
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
        std::fs::write(dir.join("token"), token).unwrap();
        std::fs::write(
            dir.join("metadata.json"),
            json!({"pid": pid, "port": port, "identifier": identifier, "product_name": "Mock"})
                .to_string(),
        )
        .unwrap();
        dir
    }

    /// The real CLI binary, with every temp-dir source pointed at this root and no
    /// inherited endpoint configuration.
    fn victauri(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_victauri"));
        for var in ["TMP", "TEMP", "TMPDIR", "XDG_RUNTIME_DIR"] {
            cmd.env(var, self.0.path());
        }
        for var in ["VICTAURI_PORT", "VICTAURI_AUTH_TOKEN", "VICTAURI_APP"] {
            cmd.env_remove(var);
        }
        cmd.args(args);
        cmd
    }
}

/// A second live process of ours, so two discovery entries can both have live owners.
struct Sleeper(Child);

impl Sleeper {
    fn spawn() -> Self {
        let child = if cfg!(windows) {
            Command::new("ping")
                .args(["-n", "120", "127.0.0.1"])
                .stdout(Stdio::null())
                .spawn()
        } else {
            Command::new("sleep").arg("120").spawn()
        };
        Self(child.expect("spawn a sleeper process"))
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Clone)]
struct MockApp {
    identifier: String,
}

fn tool_result(value: &Value) -> Value {
    json!({"content": [{"type": "text", "text": value.to_string()}]})
}

async fn mcp(State(app): State<MockApp>, body: String) -> Response {
    let v: Value = serde_json::from_str(&body).unwrap_or_else(|_| json!({}));
    let id = v.get("id").cloned();
    let result = match v["method"].as_str().unwrap_or("") {
        "initialize" => json!({"protocolVersion": "2025-03-26", "capabilities": {},
                               "serverInfo": {"name": "mock", "version": "0"}}),
        "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
        "tools/call" => {
            let value = match v["params"]["name"].as_str().unwrap_or("") {
                "get_plugin_info" => json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "tools": {"total": 35, "enabled": 35},
                    "uptime_secs": 5,
                    "app": {"identifier": app.identifier},
                }),
                "get_registry" => json!([]),
                "check_ipc_integrity" => json!({"healthy": true}),
                // The plugin's real report shape: `confirmed_ghosts` items are
                // `{name, error}`, `frontend_only` items are serialized `GhostCommand`s.
                "detect_ghost_commands" => json!({
                    "confirmed_ghosts": [
                        {"name": "get_widgetz", "error": "command get_widgetz not found"},
                        {"name": "get_widgetz", "error": "command get_widgetz not found"}
                    ],
                    "frontend_only": [
                        {"name": "set_langauge", "source": "FrontendOnly", "description": null}
                    ],
                    "reliability": "low",
                }),
                "get_memory_stats" => json!({"working_set_bytes": 1_048_576}),
                "invoke_command" => json!({"app": app.identifier}),
                _ => json!({}),
            };
            tool_result(&value)
        }
        _ => json!({}),
    };
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

/// Start a mock Victauri server; returns its port.
async fn start_mock(identifier: &str) -> u16 {
    let state = MockApp {
        identifier: identifier.to_string(),
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

/// Run the CLI off the async runtime (the mocks live on it).
async fn run(cmd: Command) -> Output {
    let mut cmd = cmd;
    tokio::task::spawn_blocking(move || cmd.output().expect("run victauri"))
        .await
        .unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_lists_ghost_command_names_including_confirmed_ghosts() {
    let iso = IsolatedTemp::new();
    let port = start_mock("com.test.ghosts").await;
    iso.write_entry(std::process::id(), port, "tok", "com.test.ghosts");

    let out = run(iso.victauri(&["check"])).await;
    let err = stderr(&out);
    assert!(out.status.success(), "check failed:\n{err}");
    assert!(
        err.contains("get_widgetz"),
        "confirmed ghosts must be listed by name:\n{err}"
    );
    assert!(
        err.contains("set_langauge"),
        "candidate ghosts must be listed by name:\n{err}"
    );
    assert_eq!(
        err.matches("get_widgetz").count(),
        1,
        "names are deduplicated:\n{err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_running_apps_are_named_and_selectable_with_app() {
    // R4-CLI1: with two apps and no VICTAURI_PORT, check/test/invoke/coverage/doctor fell back
    // to (7373, no token) and printed a 401 with "stale CLI / is your app running?" advice.
    let iso = IsolatedTemp::new();
    let sleeper = Sleeper::spawn();
    let port_a = start_mock("com.test.alpha").await;
    let port_b = start_mock("com.test.beta").await;
    iso.write_entry(std::process::id(), port_a, "tok-a", "com.test.alpha");
    iso.write_entry(sleeper.pid(), port_b, "tok-b", "com.test.beta");

    let out = run(iso.victauri(&["check"])).await;
    let err = stderr(&out);
    assert!(!out.status.success(), "must refuse to guess:\n{err}");
    assert!(
        err.contains(&format!("com.test.alpha (port {port_a}")),
        "{err}"
    );
    assert!(
        err.contains(&format!("com.test.beta (port {port_b}")),
        "{err}"
    );
    assert!(
        err.contains("--app") && err.contains("VICTAURI_APP"),
        "{err}"
    );
    assert!(
        !err.contains("Is your Tauri app running") && !err.contains("401"),
        "no misleading connection advice:\n{err}"
    );

    // `--app` selects one (exact identifier)…
    let out = run(iso.victauri(&["invoke", "whoami", "--raw", "--app", "com.test.beta"])).await;
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("com.test.beta"), "{}", stdout(&out));

    // …and so does VICTAURI_APP.
    let mut cmd = iso.victauri(&["invoke", "whoami", "--raw"]);
    cmd.env("VICTAURI_APP", "com.test.alpha");
    let out = run(cmd).await;
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("com.test.alpha"), "{}", stdout(&out));

    // A prefix is not an identifier.
    let out = run(iso.victauri(&["invoke", "whoami", "--raw", "--app", "com.test"])).await;
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("No running Victauri app matches"),
        "{}",
        stderr(&out)
    );
    drop(sleeper);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_token_without_a_port_is_not_sent_to_the_default_port() {
    // R4-TOK1: VICTAURI_AUTH_TOKEN with no VICTAURI_PORT was sent to whoever held 7373.
    let iso = IsolatedTemp::new();
    let port = start_mock("com.test.token").await;
    iso.write_entry(std::process::id(), port, "the-real-token", "com.test.token");

    // A token matching the running app's own discovery token goes to that app.
    let mut cmd = iso.victauri(&["invoke", "whoami", "--raw"]);
    cmd.env("VICTAURI_AUTH_TOKEN", "the-real-token");
    let out = run(cmd).await;
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("com.test.token"));

    // Any other token is sent nowhere, with the fix named.
    let mut cmd = iso.victauri(&["invoke", "whoami", "--raw"]);
    cmd.env("VICTAURI_AUTH_TOKEN", "some-other-token");
    let out = run(cmd).await;
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("VICTAURI_PORT"), "{err}");
}

#[test]
fn init_prints_a_hostile_identifier_on_one_escaped_line() {
    // R4-TERM1: `victauri init` printed the raw `identifier` from tauri.conf.json — a cloned
    // repo could forge a GitHub Actions annotation (`::error`) or drive the terminal (ESC).
    let iso = IsolatedTemp::new();
    let project = iso.0.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ntauri = \"2\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("tauri.conf.json"),
        json!({"identifier": "com.x\n::error file=src/main.rs::forged\u{1b}[2J"}).to_string(),
    )
    .unwrap();
    let out = iso
        .victauri(&["init", "--path", project.to_str().unwrap()])
        .output()
        .unwrap();
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert!(
        !err.lines().any(|l| l.trim_start().starts_with("::")),
        "an identifier line became a workflow command:\n{err}"
    );
    assert!(
        !err.contains('\u{1b}'),
        "raw ESC reached the terminal:\n{err:?}"
    );
    assert!(
        err.contains(r"bridge pinned to app 'com.x\n::error"),
        "{err}"
    );
}

/// R5B-DOCTOR1: `victauri doctor` printed `[FAIL]` items but always exited 0, so a CI step
/// running it could never fail. A FAIL exits 1; warnings alone do not.
#[test]
fn doctor_exits_nonzero_when_a_check_fails() {
    let iso = IsolatedTemp::new();
    // No Cargo.toml here: the very first check FAILs.
    let empty = iso.0.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let out = iso
        .victauri(&["doctor"])
        .current_dir(&empty)
        .output()
        .unwrap();
    let err = stderr(&out);
    assert!(err.contains("[FAIL]"), "{err}");
    assert_eq!(out.status.code(), Some(1), "a FAIL must exit 1:\n{err}");
}

fn write_tauri_project(dir: &std::path::Path, tauri_dep: bool) {
    std::fs::create_dir_all(dir).unwrap();
    let deps = if tauri_dep { "tauri = \"2\"\n" } else { "" };
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{deps}"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("tauri.conf.json"),
        json!({"identifier": "com.test.init"}).to_string(),
    )
    .unwrap();
}

/// R5B-INIT1: `init` canonicalizes the project root, which on Windows yields a `\?\C:\…`
/// verbatim path — and printed it that way.
#[test]
fn init_never_prints_verbatim_windows_paths() {
    let iso = IsolatedTemp::new();
    let project = iso.0.path().join("project");
    // No tauri dependency → `init` warns and names the Cargo.toml path.
    write_tauri_project(&project, false);
    let out = iso
        .victauri(&["init", "--path", project.to_str().unwrap()])
        .output()
        .unwrap();
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("Cargo.toml"), "{err}");
    assert!(!err.contains(r"\?\"), "verbatim path printed:\n{err}");
}
