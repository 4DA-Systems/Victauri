//! Regression tests for the server-robustness audit findings (F3–F6, C8, C10–C12): each one
//! drives the real `execute_tool` dispatch with a bridge configured to reproduce the finding.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::model::{CallToolResult, ContentBlock};
use victauri_core::WindowState;

use super::VictauriMcpHandler;
use crate::VictauriState;
use crate::bridge::WebviewBridge;

/// A bridge whose app directories, UI responsiveness and failure modes are set per test.
#[derive(Default)]
struct TestBridge {
    data: Option<PathBuf>,
    config: Option<PathBuf>,
    local_data: Option<PathBuf>,
    log: Option<PathBuf>,
    /// Every main-thread window query fails as a dispatch timeout would.
    ui_busy: bool,
    /// `app_data_dir` panics (a handler that reaches it panics).
    panic_on_data_dir: bool,
}

const BUSY: &str = "get_window_states did not complete on the main thread: timed out";

fn dir(d: Option<&PathBuf>) -> Result<PathBuf, String> {
    d.cloned().ok_or_else(|| "no such dir".to_string())
}

impl WebviewBridge for TestBridge {
    fn eval_webview(&self, _label: Option<&str>, _script: &str) -> Result<(), String> {
        Err("no webview in this test".to_string())
    }
    fn get_window_states(&self, label: Option<&str>) -> Vec<WindowState> {
        self.try_get_window_states(label).unwrap_or_default()
    }
    fn try_get_window_states(&self, label: Option<&str>) -> Result<Vec<WindowState>, String> {
        if self.ui_busy {
            return Err(BUSY.to_string());
        }
        Ok(
            vec![WindowState::new("main".to_string()).with_visible(true)]
                .into_iter()
                .filter(|w| label.is_none_or(|l| w.label == l))
                .collect(),
        )
    }
    fn list_window_labels(&self) -> Vec<String> {
        self.try_list_window_labels().unwrap_or_default()
    }
    fn try_list_window_labels(&self) -> Result<Vec<String>, String> {
        if self.ui_busy {
            return Err(BUSY.to_string());
        }
        Ok(vec!["main".to_string()])
    }
    fn get_native_handle(&self, _label: Option<&str>) -> Result<isize, String> {
        Err("no handle".to_string())
    }
    fn manage_window(&self, _label: Option<&str>, _action: &str) -> Result<String, String> {
        Err("no window".to_string())
    }
    fn resize_window(&self, _l: Option<&str>, _w: u32, _h: u32) -> Result<(), String> {
        Ok(())
    }
    fn move_window(&self, _l: Option<&str>, _x: i32, _y: i32) -> Result<(), String> {
        Ok(())
    }
    fn set_window_title(&self, _l: Option<&str>, _t: &str) -> Result<(), String> {
        Ok(())
    }
    fn app_data_dir(&self) -> Result<PathBuf, String> {
        assert!(!self.panic_on_data_dir, "app_data_dir exploded");
        dir(self.data.as_ref())
    }
    fn app_config_dir(&self) -> Result<PathBuf, String> {
        dir(self.config.as_ref())
    }
    fn app_log_dir(&self) -> Result<PathBuf, String> {
        dir(self.log.as_ref())
    }
    fn app_local_data_dir(&self) -> Result<PathBuf, String> {
        dir(self.local_data.as_ref())
    }
}

fn handler_with(bridge: TestBridge, state: VictauriState) -> VictauriMcpHandler {
    let mut state = state;
    state.eval_timeout = Duration::from_millis(200);
    VictauriMcpHandler::new(Arc::new(state), Arc::new(bridge))
}

fn handler(bridge: TestBridge) -> VictauriMcpHandler {
    handler_with(bridge, VictauriState::for_tests())
}

async fn call(h: &VictauriMcpHandler, tool: &str, args: serde_json::Value) -> CallToolResult {
    // Bounded so a regression that hangs fails instead of wedging the suite.
    tokio::time::timeout(Duration::from_secs(20), h.execute_tool(tool, args))
        .await
        .unwrap_or_else(|_| panic!("{tool} did not return within 20s"))
        .unwrap_or_else(|_| panic!("{tool}: dispatch rejected the arguments"))
}

fn text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| match c {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn json(r: &CallToolResult) -> serde_json::Value {
    assert_ne!(r.is_error, Some(true), "unexpected tool error: {}", text(r));
    serde_json::from_str(&text(r)).expect("tool result is JSON")
}

#[cfg(feature = "sqlite")]
fn make_db(path: &std::path::Path, table: &str) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE {table} (x INTEGER); INSERT INTO {table} VALUES (1);"
    ))
    .unwrap();
}

// ── F3: one ordered, de-duplicated root list for query_db AND db_health ──────────────────

#[cfg(feature = "sqlite")]
#[test]
fn db_roots_are_ordered_deduplicated_and_include_the_log_dir() {
    let (a, b, c) = (
        PathBuf::from("/a"),
        PathBuf::from("/b"),
        PathBuf::from("/c"),
    );
    let mut state = VictauriState::for_tests();
    state.db_search_paths = vec![b.clone()];
    let h = handler_with(
        TestBridge {
            data: Some(a.clone()),
            config: Some(b.clone()),
            local_data: Some(a.clone()),
            log: Some(c.clone()),
            ..TestBridge::default()
        },
        state,
    );
    for _ in 0..16 {
        assert_eq!(h.db_roots(), vec![b.clone(), a.clone(), c.clone()]);
    }
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn db_health_finds_a_relative_path_in_the_log_dir() {
    let data = tempfile::tempdir().unwrap();
    let log = tempfile::tempdir().unwrap();
    make_db(&log.path().join("app.db"), "logged");
    let h = handler(TestBridge {
        data: Some(data.path().to_path_buf()),
        log: Some(log.path().to_path_buf()),
        ..TestBridge::default()
    });
    let r = call(
        &h,
        "introspect",
        serde_json::json!({"action": "db_health", "db_path": "app.db"}),
    )
    .await;
    assert_eq!(json(&r)["tables"][0]["name"], "logged");
}

// ── F4: list_app_dir walks are bounded by entries EXAMINED ──────────────────────────────

#[test]
fn dir_walk_stops_at_the_visit_cap_even_when_nothing_matches() {
    let root = tempfile::tempdir().unwrap();
    for i in 0..50 {
        std::fs::write(root.path().join(format!("f{i}.txt")), "x").unwrap();
    }
    let mut walk = super::DirWalk {
        canon_base: std::fs::canonicalize(root.path()).unwrap(),
        pattern: Some("*.nomatch".to_string()),
        max_depth: 5,
        deadline: Instant::now() + Duration::from_secs(30),
        max_visited: 10,
        visited: 0,
        truncated: false,
        entries: Vec::new(),
    };
    walk.visit(root.path(), root.path(), 0);
    assert!(walk.entries.is_empty());
    assert_eq!(walk.visited, 10);
    assert!(walk.truncated, "a capped walk must say so");
}

#[tokio::test]
async fn list_app_dir_reports_a_complete_listing_as_not_truncated() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.db"), "x").unwrap();
    std::fs::write(root.path().join("b.txt"), "x").unwrap();
    let h = handler(TestBridge {
        data: Some(root.path().to_path_buf()),
        ..TestBridge::default()
    });
    let r = json(&call(&h, "list_app_dir", serde_json::json!({"pattern": "*.db"})).await);
    assert_eq!(r["count"], 1);
    assert_eq!(r["truncated"], false);
    assert_eq!(r["entries"][0]["name"], "a.db");
}

// ── F5: truncating mid-character keeps the text a text ──────────────────────────────────

#[tokio::test]
async fn read_app_file_truncated_mid_character_stays_utf8() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("notes.txt"), "é".repeat(100)).unwrap();
    let h = handler(TestBridge {
        data: Some(root.path().to_path_buf()),
        ..TestBridge::default()
    });
    let r = json(
        &call(
            &h,
            "read_app_file",
            serde_json::json!({"path": "notes.txt", "max_bytes": 11}),
        )
        .await,
    );
    assert_eq!(r["encoding"], "utf-8", "{r}");
    assert_eq!(r["content"], "é".repeat(5));
    assert_eq!(r["file"]["truncated"], true);
}

#[tokio::test]
async fn read_app_file_invalid_utf8_is_still_base64() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("bin.dat"), [0x66, 0xff, 0x66, 0x66]).unwrap();
    let h = handler(TestBridge {
        data: Some(root.path().to_path_buf()),
        ..TestBridge::default()
    });
    let r = json(
        &call(
            &h,
            "read_app_file",
            serde_json::json!({"path": "bin.dat", "max_bytes": 3}),
        )
        .await,
    );
    assert_eq!(r["encoding"], "base64");
}

// ── F6: no existence oracle through `..` or a symlink inside a root ─────────────────────

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn query_db_rejects_parent_components_in_absolute_paths() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    make_db(&root.path().join("app.db"), "t");
    let h = handler(TestBridge {
        data: Some(root.path().to_path_buf()),
        ..TestBridge::default()
    });
    let sneaky = root.path().join("sub").join("..").join("app.db");
    let r = call(
        &h,
        "query_db",
        serde_json::json!({"query": "SELECT 1", "path": sneaky.to_string_lossy()}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("'..' is rejected"), "{}", text(&r));
}

#[cfg(feature = "sqlite")]
#[cfg(unix)]
#[tokio::test]
async fn a_symlink_out_of_a_root_is_no_existence_oracle() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    make_db(&outside.path().join("secret.db"), "t");
    std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
    let h = handler(TestBridge {
        data: Some(root.path().to_path_buf()),
        ..TestBridge::default()
    });

    // query_db, absolute and relative: an existing and a missing outside file answer alike.
    for (existing, missing) in [
        (
            root.path()
                .join("link/secret.db")
                .to_string_lossy()
                .into_owned(),
            root.path()
                .join("link/nope.db")
                .to_string_lossy()
                .into_owned(),
        ),
        ("link/secret.db".to_string(), "link/nope.db".to_string()),
    ] {
        let a = call(
            &h,
            "query_db",
            serde_json::json!({"query": "SELECT 1", "path": existing}),
        )
        .await;
        let b = call(
            &h,
            "query_db",
            serde_json::json!({"query": "SELECT 1", "path": missing}),
        )
        .await;
        assert_eq!(a.is_error, Some(true));
        assert_eq!(
            text(&a).replace("secret.db", "X"),
            text(&b).replace("nope.db", "X")
        );
    }

    // read_app_file and list_app_dir: both refused as traversal.
    for path in ["link/secret.db", "link/nope.db"] {
        let r = call(&h, "read_app_file", serde_json::json!({"path": path})).await;
        assert!(
            text(&r).contains("path traversal not allowed"),
            "{path}: {}",
            text(&r)
        );
    }
    for path in ["link", "link/missing-subdir"] {
        let r = call(&h, "list_app_dir", serde_json::json!({"path": path})).await;
        assert!(
            text(&r).contains("path traversal not allowed"),
            "{path}: {}",
            text(&r)
        );
    }
    // A genuinely missing path inside the root is still a normal "exists: false".
    let r = json(
        &call(
            &h,
            "list_app_dir",
            serde_json::json!({"path": "nothing-here"}),
        )
        .await,
    );
    assert_eq!(r["exists"], false);
}

// ── C8: look-back windows saturate; a panicking handler is an error result ──────────────

#[tokio::test]
async fn huge_look_back_windows_do_not_panic() {
    let h = handler(TestBridge::default());
    let r = call(
        &h,
        "introspect",
        serde_json::json!({"action": "event_bus", "args": {"since_ms": u64::MAX}}),
    )
    .await;
    assert_ne!(r.is_error, Some(true), "{}", text(&r));
    for action in ["summary", "last_action", "diff"] {
        let r = call(
            &h,
            "explain",
            serde_json::json!({"action": action, "seconds": 10_000_000_000_000_u64}),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{action}: {}", text(&r));
    }
    let r = call(
        &h,
        "wait_for",
        serde_json::json!({"condition": "event", "value": "never", "timeout_ms": 50,
                           "since_ms": 9_000_000_000_000_000_u64}),
    )
    .await;
    assert!(text(&r).contains("timeout after 50ms"), "{}", text(&r));
}

/// `since_ms` above `i64::MAX` used to wrap negative and put the baseline in the FUTURE, so
/// an event already on the bus was never matched.
#[tokio::test]
async fn wait_for_event_with_a_huge_look_back_sees_past_events() {
    let h = handler(TestBridge::default());
    h.state
        .event_bus
        .push(crate::introspection::CapturedTauriEvent {
            name: "job-done".to_string(),
            payload: String::new(),
            timestamp: (chrono::Utc::now() - chrono::TimeDelta::seconds(5))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        });
    let r = json(
        &call(
            &h,
            "wait_for",
            serde_json::json!({"condition": "event", "value": "job-done", "timeout_ms": 200,
                               "since_ms": i64::MAX as u64 + 1}),
        )
        .await,
    );
    assert_eq!(r["ok"], true, "{r}");
}

#[tokio::test]
async fn a_panicking_handler_returns_an_internal_error_over_rest() {
    let h = handler(TestBridge {
        panic_on_data_dir: true,
        ..TestBridge::default()
    });
    let r = call(&h, "list_app_dir", serde_json::json!({})).await;
    assert_eq!(r.is_error, Some(true));
    let t = text(&r);
    assert!(
        t.contains("handler panicked") && t.contains("app_data_dir exploded"),
        "{t}"
    );
    // The server keeps serving.
    let r = call(&h, "get_plugin_info", serde_json::json!({})).await;
    assert_ne!(r.is_error, Some(true));
}

#[tokio::test]
async fn a_panicking_handler_returns_an_internal_error_over_mcp() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let app = super::build_app(
        Arc::new(VictauriState::for_tests()),
        Arc::new(TestBridge {
            panic_on_data_dir: true,
            ..TestBridge::default()
        }),
    );
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "list_app_dir", "arguments": {}},
    });
    let req = axum::http::Request::post("/mcp")
        .header("host", "127.0.0.1:7373")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-06-18")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let resp = tokio::time::timeout(Duration::from_secs(20), app.oneshot(req))
        .await
        .expect("an MCP call to a panicking handler must still be answered")
        .unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&bytes);
    assert!(body.contains("handler panicked"), "{body}");
}

// ── C10: wait_for(event) never sleeps past its deadline ─────────────────────────────────

#[tokio::test]
async fn wait_for_event_returns_at_its_timeout_whatever_the_poll_interval() {
    let h = handler(TestBridge::default());
    for poll in [3_000_u64, u64::MAX] {
        let started = Instant::now();
        let r = call(
            &h,
            "wait_for",
            serde_json::json!({"condition": "event", "value": "never", "timeout_ms": 100,
                               "poll_ms": poll}),
        )
        .await;
        assert!(text(&r).contains("timeout after 100ms"), "{}", text(&r));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "poll_ms={poll} took {:?}",
            started.elapsed()
        );
    }
}

// ── C11: a busy UI is reported as busy, never as "no windows" ───────────────────────────

#[tokio::test]
async fn a_busy_ui_is_not_reported_as_missing_or_empty_windows() {
    let h = handler(TestBridge {
        ui_busy: true,
        ..TestBridge::default()
    });
    for args in [
        serde_json::json!({"action": "get_state", "label": "main"}),
        serde_json::json!({"action": "get_state"}),
        serde_json::json!({"action": "list"}),
        serde_json::json!({"action": "introspectability"}),
    ] {
        let r = call(&h, "window", args.clone()).await;
        assert_eq!(r.is_error, Some(true), "{args}: {}", text(&r));
        assert!(text(&r).contains("UI thread busy"), "{args}: {}", text(&r));
        assert!(!text(&r).contains("not found"), "{args}: {}", text(&r));
    }
    let caps = json(
        &call(
            &h,
            "introspect",
            serde_json::json!({"action": "capabilities"}),
        )
        .await,
    );
    assert!(caps["live_windows"].is_null());
    assert!(
        caps["live_windows_error"]
            .as_str()
            .is_some_and(|e| e.contains("UI thread busy"))
    );
    let r = call(&h, "screenshot", serde_json::json!({})).await;
    assert!(text(&r).contains("UI thread busy"), "{}", text(&r));
}

// ── C12: app probes run off the executor, bounded and panic-isolated ────────────────────

#[tokio::test]
async fn app_state_probes_are_bounded_and_panic_isolated() {
    let state = VictauriState::for_tests();
    state.probes.register(
        "stuck",
        Arc::new(|| {
            std::thread::sleep(Duration::from_secs(4));
            serde_json::json!({"late": true})
        }),
    );
    state
        .probes
        .register("broken", Arc::new(|| panic!("probe state poisoned")));
    state
        .probes
        .register("fine", Arc::new(|| serde_json::json!({"depth": 3})));
    let h = handler_with(TestBridge::default(), state);

    let started = Instant::now();
    let r = call(&h, "app_state", serde_json::json!({"probe": "stuck"})).await;
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("did not finish"), "{}", text(&r));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );

    let r = call(&h, "app_state", serde_json::json!({"probe": "broken"})).await;
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("probe state poisoned"), "{}", text(&r));

    let r = json(&call(&h, "app_state", serde_json::json!({"probe": "fine"})).await);
    assert_eq!(r["depth"], 3);
}

/// R4-BLK1: a hung probe leaves its blocking thread running past the deadline, so repeated
/// calls to it used to pile up one leaked thread each. Probe executions are capped: once every
/// slot is held by a still-running probe, a call is refused at once instead of leaking another.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hung_probes_cannot_pile_up_blocking_threads() {
    let state = VictauriState::for_tests();
    state.probes.register(
        "stuck",
        Arc::new(|| {
            std::thread::sleep(Duration::from_secs(3));
            serde_json::json!({"late": true})
        }),
    );
    let h = Arc::new(handler_with(TestBridge::default(), state));
    let calls: Vec<_> = (0..super::MAX_CONCURRENT_PROBES + 2)
        .map(|_| {
            let h = Arc::clone(&h);
            tokio::spawn(async move {
                let started = Instant::now();
                let r = call(&h, "app_state", serde_json::json!({"probe": "stuck"})).await;
                (text(&r), started.elapsed())
            })
        })
        .collect();
    let mut busy = 0;
    for c in calls {
        let (t, took) = c.await.unwrap();
        if t.contains("busy") {
            busy += 1;
            assert!(
                took < Duration::from_millis(500),
                "a refusal must be immediate: {took:?}"
            );
        } else {
            assert!(t.contains("did not finish"), "{t}");
        }
    }
    assert_eq!(busy, 2, "calls beyond the cap must be refused");
}

/// R4-BLK1: `read_app_file` reads under a deadline and a small concurrency cap, so a read that
/// blocks (a FIFO or device swapped in after the checks) cannot hang the call or pile up threads.
#[tokio::test]
async fn read_app_file_is_refused_while_every_read_slot_is_held() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("notes.txt"), "hello").unwrap();
    let h = handler(TestBridge {
        data: Some(root.path().to_path_buf()),
        ..TestBridge::default()
    });
    let held: Vec<_> = (0..super::MAX_CONCURRENT_FILE_READS)
        .map(|_| Arc::clone(&h.file_slots).try_acquire_owned().unwrap())
        .collect();
    let r = call(
        &h,
        "read_app_file",
        serde_json::json!({"path": "notes.txt"}),
    )
    .await;
    assert!(text(&r).contains("busy"), "{}", text(&r));
    drop(held);
    let r = json(
        &call(
            &h,
            "read_app_file",
            serde_json::json!({"path": "notes.txt"}),
        )
        .await,
    );
    assert_eq!(r["content"], "hello");
}

/// A read that blocks (here: a FIFO with no writer, which `open` waits on) returns at the
/// deadline instead of hanging the call.
#[cfg(unix)]
#[tokio::test]
async fn a_blocking_file_read_returns_at_its_deadline() {
    let root = tempfile::tempdir().unwrap();
    let fifo = root.path().join("pipe");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status();
    if !made.is_ok_and(|s| s.success()) {
        eprintln!("SKIP: mkfifo unavailable");
        return;
    }
    let h = handler(TestBridge::default());
    let started = Instant::now();
    let r = tokio::time::timeout(
        Duration::from_secs(20),
        h.read_regular_file_bounded(fifo, 16),
    )
    .await
    .expect("a blocked read must return at its own deadline");
    let err = r.expect_err("a FIFO read cannot succeed");
    assert!(err.contains("did not finish"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// The OPENED file is checked: a directory (or FIFO/device) swapped in is refused.
#[cfg(unix)]
#[test]
fn a_non_regular_file_is_refused_after_open() {
    let root = tempfile::tempdir().unwrap();
    let err = super::read_regular_file(root.path(), 16).expect_err("a directory is not a file");
    assert!(err.contains("not a regular file"), "{err}");
}

/// A refused query is refused for what it is, before any database is looked up. Seen live on
/// Windows: the demo app has no application database, so a DELETE came back as "only `WebView`
/// internal databases were found" instead of "read-only" (the adversarial E2E suite, which CI
/// runs on Linux only, failed there). Here the app has no data directory at all.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_refused_query_is_refused_before_any_database_is_resolved() {
    let h = handler(TestBridge::default());
    for (sql, expect) in [
        ("DELETE FROM x", "read-only"),
        ("PRAGMA journal_mode = WAL", "PRAGMA writes"),
        ("PRAGMA wal_checkpoint", "side-effecting PRAGMAs"),
        ("SELECT 1; DROP TABLE x", "stacked"),
    ] {
        let r = call(&h, "query_db", serde_json::json!({ "query": sql })).await;
        assert_eq!(r.is_error, Some(true), "{sql}");
        assert!(text(&r).contains(expect), "{sql}: {}", text(&r));
    }
}
