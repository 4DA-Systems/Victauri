//! Tools target what was asked, report what happened, and privacy controls hold. Each test drives
//! the real `execute_tool` dispatch (or the real HTTP router) with a bridge that answers the
//! page's side of the protocol.

use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};

use serde_json::json;
use tower::ServiceExt;
use victauri_core::WindowState;

use super::*;

type Responder = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// A two-window app: answers the liveness probe (unless `dead`), every eval wrapper through
/// `respond`, and records what the native window calls were asked to do.
#[derive(Clone)]
struct AppBridge {
    pending: crate::PendingCallbacks,
    respond: Responder,
    /// The page stopped answering: a loop, a blocking command, or a navigation out of the app.
    dead: Arc<AtomicBool>,
    /// A navigation does not revive a dead page: it waits behind the stopped script (`WebKit`).
    stays_dead: Arc<AtomicBool>,
    /// `(pid, ticks per CPU reading)` of the page processes that are the app's children (Linux).
    web_processes: Arc<StdMutex<Vec<(u32, u64)>>>,
    cpu_readings: Arc<AtomicUsize>,
    ended: Arc<StdMutex<Vec<u32>>>,
    /// `WebKit` has not noticed that the web process is gone: the next navigation is lost.
    drop_navigation: Arc<AtomicBool>,
    /// The other window's page, apart from `dead`: `Some(answers)`.
    sibling_answers: Arc<StdMutex<Option<bool>>>,
    /// The window never had a bridge that could call back (no `victauri:default`).
    states: Arc<StdMutex<Vec<WindowState>>>,
    /// The size the OS really gives a resized window (`None`: what was asked for).
    os_size_limit: Option<(u32, u32)>,
    /// Linux and macOS apply a resize or move later: the change lands after this long.
    os_apply_delay: Option<Duration>,
    /// The window manager ignores a resize or move altogether.
    os_ignores_geometry: bool,
    evals: Arc<StdMutex<Vec<(Option<String>, String)>>>,
    /// `(label, what)` of every native window call.
    native: Arc<StdMutex<Vec<(Option<String>, String)>>>,
}

impl AppBridge {
    fn new(state: &Arc<VictauriState>, respond: Responder) -> Self {
        Self {
            pending: state.pending_evals.clone(),
            respond,
            dead: Arc::default(),
            stays_dead: Arc::default(),
            web_processes: Arc::default(),
            cpu_readings: Arc::default(),
            ended: Arc::default(),
            drop_navigation: Arc::default(),
            sibling_answers: Arc::default(),
            states: Arc::new(StdMutex::new(vec![
                window_state("main", "http://tauri.localhost/index.html"),
                window_state("notifications", "http://tauri.localhost/notifications.html"),
            ])),
            os_size_limit: None,
            os_apply_delay: None,
            os_ignores_geometry: false,
            evals: Arc::default(),
            native: Arc::default(),
        }
    }

    fn answering(state: &Arc<VictauriState>, body: &str) -> Self {
        let body = body.to_string();
        Self::new(state, Arc::new(move |_| Some(body.clone())))
    }

    fn answer(&self, id: String, body: String) {
        let pending = self.pending.clone();
        std::thread::spawn(move || {
            if let Some(tx) = pending.blocking_lock().remove(&id) {
                let _ = tx.send(body);
            }
        });
    }

    fn native_calls(&self) -> Vec<(Option<String>, String)> {
        self.native.lock().unwrap().clone()
    }

    fn evals_containing(&self, needle: &str) -> usize {
        self.evals
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| s.contains(needle))
            .count()
    }

    /// What the OS does with a resize or move, when it gets to it.
    fn apply_geometry(&self, change: impl Fn(&mut WindowState) + Send + 'static) {
        if self.os_ignores_geometry {
            return;
        }
        let states = Arc::clone(&self.states);
        let apply = move || {
            for state in states.lock().unwrap().iter_mut() {
                change(state);
            }
        };
        match self.os_apply_delay {
            Some(delay) => {
                std::thread::spawn(move || {
                    std::thread::sleep(delay);
                    apply();
                });
            }
            None => apply(),
        }
    }

    fn set_window_url(&self, label: &str, url: &str) {
        for state in self.states.lock().unwrap().iter_mut() {
            if state.label == label {
                *state = window_state(label, url);
            }
        }
    }
}

fn window_state(label: &str, url: &str) -> WindowState {
    WindowState::new(label.to_string())
        .with_title(String::new())
        .with_url(url.to_string())
        .with_visible(true)
        .with_position(0, 0)
        .with_size(800, 600)
}

fn quoted_id_after(script: &str, marker: &str) -> Option<String> {
    let start = script.find(marker)? + marker.len();
    script.get(start..start + 36).map(str::to_string)
}

impl WebviewBridge for AppBridge {
    fn eval_webview(&self, label: Option<&str>, script: &str) -> Result<(), String> {
        self.evals
            .lock()
            .unwrap()
            .push((label.map(str::to_string), script.to_string()));
        if label == Some("notifications")
            && let Some(answers) = *self.sibling_answers.lock().unwrap()
        {
            if answers && let Some(id) = quoted_id_after(script, "id:\"") {
                self.answer(id, "\"probe_ok:page-1\"".to_string());
            }
            return Ok(());
        }
        if script.contains("location.replace(") && !self.stays_dead.load(Ordering::SeqCst) {
            self.dead.store(false, Ordering::SeqCst);
        }
        if self.dead.load(Ordering::SeqCst) {
            return Ok(());
        }
        if script.contains("probe_ok") {
            if let Some(id) = quoted_id_after(script, "id:\"") {
                self.answer(id, "\"probe_ok:page-1\"".to_string());
            }
        } else if let Some(id) = quoted_id_after(script, "__vic = { id: \"")
            && let Some(body) = (self.respond)(script)
        {
            self.answer(id, body);
        }
        Ok(())
    }
    fn eval_webview_resolved(&self, label: Option<&str>, script: &str) -> Result<String, String> {
        self.eval_webview(label, script)?;
        Ok(label.unwrap_or("main").to_string())
    }
    fn get_window_states(&self, label: Option<&str>) -> Vec<WindowState> {
        self.states
            .lock()
            .unwrap()
            .iter()
            .filter(|s| label.is_none_or(|l| s.label == l))
            .cloned()
            .collect()
    }
    fn list_window_labels(&self) -> Vec<String> {
        vec!["main".to_string(), "notifications".to_string()]
    }
    fn get_native_handle(&self, _l: Option<&str>) -> Result<isize, String> {
        Err("no handle".to_string())
    }
    fn manage_window(&self, label: Option<&str>, action: &str) -> Result<String, String> {
        self.native
            .lock()
            .unwrap()
            .push((label.map(str::to_string), format!("manage {action}")));
        Ok(format!("{action} executed"))
    }
    fn resize_window(&self, label: Option<&str>, w: u32, h: u32) -> Result<(), String> {
        self.native
            .lock()
            .unwrap()
            .push((label.map(str::to_string), format!("resize {w}x{h}")));
        let (w, h) = match self.os_size_limit {
            Some((mw, mh)) => (w.min(mw), h.min(mh)),
            None => (w, h),
        };
        let label = label.map(str::to_string);
        self.apply_geometry(move |state| {
            if label.as_deref().is_none_or(|l| state.label == l) {
                *state = state.clone().with_size(w, h);
            }
        });
        Ok(())
    }
    fn move_window(&self, label: Option<&str>, x: i32, y: i32) -> Result<(), String> {
        self.native
            .lock()
            .unwrap()
            .push((label.map(str::to_string), format!("move {x},{y}")));
        let label = label.map(str::to_string);
        self.apply_geometry(move |state| {
            if label.as_deref().is_none_or(|l| state.label == l) {
                *state = state.clone().with_position(x, y);
            }
        });
        Ok(())
    }
    fn set_window_title(&self, label: Option<&str>, title: &str) -> Result<(), String> {
        self.native
            .lock()
            .unwrap()
            .push((label.map(str::to_string), format!("title {title}")));
        Ok(())
    }
    fn navigate_webview(&self, label: Option<&str>, url: &str) -> Result<(), String> {
        self.native
            .lock()
            .unwrap()
            .push((label.map(str::to_string), format!("navigate {url}")));
        if self.drop_navigation.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        if !url.ends_with("/health") && !self.stays_dead.load(Ordering::SeqCst) {
            self.dead.store(false, Ordering::SeqCst);
        }
        Ok(())
    }
    fn app_entry_url(&self) -> Option<String> {
        Some("http://tauri.localhost/".to_string())
    }
    fn web_main_thread_cpu(&self) -> Vec<(u32, u64)> {
        let reading = self.cpu_readings.fetch_add(1, Ordering::SeqCst) as u64;
        self.web_processes
            .lock()
            .unwrap()
            .iter()
            .map(|(pid, ticks)| (*pid, reading * ticks))
            .collect()
    }
    /// A new process serves the next navigation, which the page answers.
    fn end_web_process(&self, pid: u32) -> Result<(), String> {
        self.ended.lock().unwrap().push(pid);
        self.stays_dead.store(false, Ordering::SeqCst);
        self.drop_navigation.store(true, Ordering::SeqCst);
        self.set_window_url("main", "");
        Ok(())
    }
}

fn state() -> Arc<VictauriState> {
    let mut s = VictauriState::for_tests();
    s.eval_timeout = Duration::from_secs(5);
    Arc::new(s)
}

fn state_with(privacy: crate::privacy::PrivacyConfig) -> Arc<VictauriState> {
    let mut s = VictauriState::for_tests();
    s.eval_timeout = Duration::from_secs(5);
    s.privacy = privacy;
    Arc::new(s)
}

fn handler(state: &Arc<VictauriState>, bridge: &AppBridge) -> VictauriMcpHandler {
    VictauriMcpHandler::new(Arc::clone(state), Arc::new(bridge.clone()))
}

async fn call(h: &VictauriMcpHandler, tool: &str, args: serde_json::Value) -> CallToolResult {
    tokio::time::timeout(Duration::from_secs(30), h.execute_tool(tool, args))
        .await
        .unwrap_or_else(|_| panic!("{tool} did not return within 30s"))
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

fn ok_json(r: &CallToolResult) -> serde_json::Value {
    assert_ne!(r.is_error, Some(true), "unexpected tool error: {}", text(r));
    serde_json::from_str(&text(r)).unwrap_or_else(|e| panic!("not JSON ({e}): {}", text(r)))
}

fn err_text(r: &CallToolResult) -> String {
    assert_eq!(r.is_error, Some(true), "expected a tool error: {}", text(r));
    text(r)
}

// ── Window targets ────────────────────────────────────────────────────────────────────────

/// Serde drops an unknown field, so a tool whose target field lacks an alias silently acts on
/// the default window. For `window` that closed the main window when asked to close another.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn window_tool_acts_on_the_named_window_under_every_spelling() {
    for key in ["label", "webview_label", "window_label", "window"] {
        let st = state();
        let bridge = AppBridge::answering(&st, "null");
        let h = handler(&st, &bridge);
        let r = call(
            &h,
            "window",
            json!({"action": "manage", "manage_action": "close", key: "notifications"}),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{}", text(&r));
        assert_eq!(
            bridge.native_calls(),
            vec![(
                Some("notifications".to_string()),
                "manage close".to_string()
            )],
            "key `{key}` must target the named window"
        );
    }
}

fn target_of<T: serde::de::DeserializeOwned>(
    extra: &serde_json::Value,
    key: &str,
    read: fn(&T) -> Option<&str>,
) -> Option<String> {
    let mut body = extra.clone();
    body[key] = json!("other");
    let parsed: T = serde_json::from_value(body).unwrap_or_else(|e| panic!("{key}: {e}"));
    read(&parsed).map(str::to_string)
}

macro_rules! assert_target_spellings {
    ($ty:ty, $extra:expr, $field:ident) => {
        for key in ["webview_label", "window_label", "window"] {
            assert_eq!(
                target_of::<$ty>(&$extra, key, |p| p.$field.as_deref()).as_deref(),
                Some("other"),
                "{}: `{key}` must set the window target",
                stringify!($ty)
            );
        }
    };
}

#[test]
fn every_param_struct_with_a_window_target_accepts_all_three_spellings() {
    assert_target_spellings!(EvalJsParams, json!({"code": "1"}), webview_label);
    assert_target_spellings!(SnapshotParams, json!({}), webview_label);
    assert_target_spellings!(ScreenshotParams, json!({}), window_label);
    assert_target_spellings!(InvokeCommandParams, json!({"command": "c"}), webview_label);
    assert_target_spellings!(FindElementsParams, json!({}), webview_label);
    assert_target_spellings!(DiagnosticsParams, json!({}), webview_label);
    assert_target_spellings!(
        SemanticAssertParams,
        json!({"expression": "1", "condition": "truthy"}),
        webview_label
    );
    assert_target_spellings!(
        WaitForParams,
        json!({"condition": "url", "value": "x"}),
        webview_label
    );
    assert_target_spellings!(
        VerifyStateParams,
        json!({"frontend_expr": "1"}),
        webview_label
    );
    assert_target_spellings!(GhostCommandParams, json!({}), webview_label);
    assert_target_spellings!(IpcIntegrityParams, json!({}), webview_label);
    assert_target_spellings!(InteractParams, json!({"action": "click"}), webview_label);
    assert_target_spellings!(InputParams, json!({"action": "fill"}), webview_label);
    assert_target_spellings!(WindowParams, json!({"action": "list"}), label);
    assert_target_spellings!(StorageParams, json!({"action": "get"}), webview_label);
    assert_target_spellings!(NavigateParams, json!({"action": "go_back"}), webview_label);
    assert_target_spellings!(RecordingParams, json!({"action": "flush"}), webview_label);
    assert_target_spellings!(
        InspectParams,
        json!({"action": "get_styles"}),
        webview_label
    );
    assert_target_spellings!(CssParams, json!({"action": "remove"}), webview_label);
    assert_target_spellings!(RouteParams, json!({"action": "list"}), webview_label);
    assert_target_spellings!(TraceParams, json!({"action": "status"}), webview_label);
    assert_target_spellings!(AnimationParams, json!({"action": "list"}), webview_label);
    assert_target_spellings!(LogsParams, json!({"action": "console"}), webview_label);
    assert_target_spellings!(
        IntrospectParams,
        json!({"action": "coverage"}),
        webview_label
    );
    assert_target_spellings!(ExplainParams, json!({"action": "diff"}), webview_label);
}

/// The table above only covers the structs it names: this catches a NEW window target added
/// without the aliases (or without an entry above).
#[test]
fn no_window_target_field_is_declared_without_its_aliases() {
    let sources = [
        include_str!("webview_params.rs"),
        include_str!("window_params.rs"),
        include_str!("other_params.rs"),
        include_str!("verification_params.rs"),
        include_str!("backend_params.rs"),
        include_str!("compound_params.rs"),
        include_str!("introspection_params.rs"),
    ];
    let mut checked = 0;
    for source in sources {
        let lines: Vec<&str> = source.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            if !(t == "pub webview_label: Option<String>,"
                || t == "pub window_label: Option<String>,")
            {
                continue;
            }
            checked += 1;
            let attr = lines[i.saturating_sub(3)..i].join(" ");
            assert!(
                attr.contains("alias = \"window\"")
                    && (attr.contains("alias = \"window_label\"")
                        || attr.contains("alias = \"webview_label\"")),
                "window target without all three spellings: {t} (after: {attr})"
            );
        }
    }
    assert!(
        checked >= 24,
        "the scan found only {checked} window targets"
    );
}

// ── navigate ──────────────────────────────────────────────────────────────────────────────

/// A JS `location.href` is one-way: once the page has no bridge (a closed port, a page outside
/// the app) nothing can run JS to come back, and only an app restart recovered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn go_to_out_of_the_app_warns_and_a_later_go_to_recovers_the_dead_page() {
    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
    let h = handler(&st, &bridge);

    let out = ok_json(
        &call(
            &h,
            "navigate",
            json!({"action": "go_to", "url": "http://127.0.0.1:1/"}),
        )
        .await,
    );
    assert_eq!(out["ok"], true);
    let warning = out["warning"].as_str().unwrap_or_default();
    assert!(warning.contains("http://127.0.0.1:1"), "{out}");
    assert!(warning.contains("http://tauri.localhost"), "{out}");

    // The page is gone: nothing answers JS any more.
    bridge.dead.store(true, Ordering::SeqCst);
    bridge.set_window_url("main", "http://127.0.0.1:1/");
    let back = ok_json(
        &call(
            &h,
            "navigate",
            json!({"action": "go_to", "url": "http://tauri.localhost/index.html"}),
        )
        .await,
    );
    assert_eq!(back["ok"], true);
    assert!(
        back.get("warning").is_none(),
        "the app's own origin: {back}"
    );
    assert_eq!(
        bridge.native_calls().last().map(|(_, c)| c.as_str()),
        Some("navigate http://tauri.localhost/index.html")
    );
    // A same-site navigation never lands on a page stuck in a loop: it leaves through a page on
    // another site first.
    let calls = bridge.native_calls();
    let detour = &calls[calls.len() - 2].1;
    assert!(
        detour.starts_with("navigate http://127.0.0.1:") && detour.ends_with("/health"),
        "{calls:?}"
    );
    // A page that answers keeps its history: no detour.
    assert_eq!(calls[0].1, "navigate http://127.0.0.1:1/", "{calls:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn go_back_on_a_dead_page_returns_to_the_last_url_inside_the_app() {
    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
    let h = handler(&st, &bridge);
    // Answered once, so the window has a working bridge; then navigated out and went dead.
    assert_ne!(
        call(&h, "get_diagnostics", json!({})).await.is_error,
        Some(true)
    );
    let _ = call(
        &h,
        "navigate",
        json!({"action": "go_to", "url": "http://127.0.0.1:1/"}),
    )
    .await;
    bridge.dead.store(true, Ordering::SeqCst);
    bridge.set_window_url("main", "http://127.0.0.1:1/");

    let out = ok_json(&call(&h, "navigate", json!({"action": "go_back"})).await);
    assert_eq!(out["ok"], true);
    assert_eq!(out["recovered"], true, "{out}");
    assert_eq!(out["url"], "http://tauri.localhost/index.html", "{out}");
    assert_eq!(
        bridge.native_calls().last().map(|(_, c)| c.as_str()),
        Some("navigate http://tauri.localhost/index.html")
    );
}

/// On `WebKit` a navigation waits behind a stopped page's script, and the window's URL already
/// reads the new one (measured on `WebKitGTK` 2.52: a "recovered" page stayed dead until its loop ended). So
/// `recovered` is what the page does afterwards, not that a navigation was sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn go_back_does_not_claim_a_recovery_the_page_does_not_show() {
    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
    let h = handler(&st, &bridge);
    assert_ne!(
        call(&h, "get_diagnostics", json!({})).await.is_error,
        Some(true)
    );
    bridge.dead.store(true, Ordering::SeqCst);
    bridge.stays_dead.store(true, Ordering::SeqCst);
    let msg = err_text(&call(&h, "navigate", json!({"action": "go_back"})).await);
    assert!(msg.contains("still does not answer"), "{msg}");
    assert!(msg.contains("http://tauri.localhost/index.html"), "{msg}");
}

/// `WebKitGTK`: the only way off a page spinning in a loop is to end its web process, and the
/// navigation after that lands. A page that does not use the CPU is not "spinning".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn go_back_ends_the_one_web_process_spinning_flat_out() {
    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
    // Between the two readings: 100 000 ticks is flat out however long recovery took.
    *bridge.web_processes.lock().unwrap() = vec![(11, 1), (22, 100_000), (33, 1)];
    let h = handler(&st, &bridge);
    assert_ne!(
        call(&h, "get_diagnostics", json!({})).await.is_error,
        Some(true)
    );
    bridge.dead.store(true, Ordering::SeqCst);
    bridge.stays_dead.store(true, Ordering::SeqCst);
    let out = ok_json(&call(&h, "navigate", json!({"action": "go_back"})).await);
    assert_eq!(out["recovered"], true, "{out}");
    assert_eq!(*bridge.ended.lock().unwrap(), vec![22]);
    assert_eq!(out["ended_web_process"], 22, "{out}");
    let calls = bridge.native_calls();
    let after_the_detour: Vec<&str> = calls
        .iter()
        .map(|(_, c)| c.as_str())
        .skip_while(|c| !c.ends_with("/health"))
        .skip(1)
        .collect();
    assert_eq!(
        after_the_detour,
        vec![
            "navigate http://tauri.localhost/index.html",
            "navigate http://tauri.localhost/index.html",
            "navigate http://tauri.localhost/index.html",
        ],
        "sent before the process ended, then lost, then sent again: the last one lands"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_web_process_is_ended_unless_one_alone_spun() {
    for processes in [
        vec![(11, 1), (33, 1)],             // a blocked page: nothing spins
        vec![(11, 100_000), (33, 100_000)], // which window is which?
    ] {
        let st = state();
        let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
        *bridge.web_processes.lock().unwrap() = processes;
        let h = handler(&st, &bridge);
        assert_ne!(
            call(&h, "get_diagnostics", json!({})).await.is_error,
            Some(true)
        );
        bridge.dead.store(true, Ordering::SeqCst);
        bridge.stays_dead.store(true, Ordering::SeqCst);
        let msg = err_text(&call(&h, "navigate", json!({"action": "go_back"})).await);
        assert!(msg.contains("still does not answer"), "{msg}");
        assert!(bridge.ended.lock().unwrap().is_empty());
    }
}

/// Nothing maps a web process to its window, so the one spinning could be another window's. A
/// sibling that has answered before says which: silent now, the process may well be its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_web_process_is_ended_only_while_the_other_windows_answer() {
    for sibling_answers in [false, true] {
        let st = state();
        let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
        *bridge.web_processes.lock().unwrap() = vec![(22, 100_000)];
        *bridge.sibling_answers.lock().unwrap() = Some(sibling_answers);
        let h = handler(&st, &bridge);
        assert_ne!(
            call(&h, "get_diagnostics", json!({})).await.is_error,
            Some(true)
        );
        h.answered_windows
            .lock()
            .unwrap()
            .insert("notifications".to_string(), None);
        bridge.dead.store(true, Ordering::SeqCst);
        bridge.stays_dead.store(true, Ordering::SeqCst);
        let result = call(&h, "navigate", json!({"action": "go_back"})).await;
        if sibling_answers {
            assert_eq!(ok_json(&result)["recovered"], true);
            assert_eq!(*bridge.ended.lock().unwrap(), vec![22]);
        } else {
            assert!(err_text(&result).contains("still does not answer"));
            assert!(bridge.ended.lock().unwrap().is_empty());
        }
    }
}

/// `go_to` says the same of what it ended, and neither reports a process it left alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn go_to_says_which_web_process_it_ended() {
    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
    *bridge.web_processes.lock().unwrap() = vec![(22, 100_000)];
    let h = handler(&st, &bridge);
    assert_ne!(
        call(&h, "get_diagnostics", json!({})).await.is_error,
        Some(true)
    );
    let target = json!({"action": "go_to", "url": "http://tauri.localhost/index.html"});
    let out = ok_json(&call(&h, "navigate", target.clone()).await);
    assert!(out.get("ended_web_process").is_none(), "{out}");
    bridge.dead.store(true, Ordering::SeqCst);
    bridge.stays_dead.store(true, Ordering::SeqCst);
    let out = ok_json(&call(&h, "navigate", target).await);
    assert_eq!(out["ended_web_process"], 22, "{out}");
}

/// A page that answers is never helped along: no process is ended, whatever the CPU says.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_that_comes_back_keeps_its_process() {
    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
    *bridge.web_processes.lock().unwrap() = vec![(22, 100_000)];
    let h = handler(&st, &bridge);
    assert_ne!(
        call(&h, "get_diagnostics", json!({})).await.is_error,
        Some(true)
    );
    bridge.dead.store(true, Ordering::SeqCst);
    let out = ok_json(&call(&h, "navigate", json!({"action": "go_back"})).await);
    assert_eq!(out["recovered"], true, "{out}");
    assert!(bridge.ended.lock().unwrap().is_empty());
    assert!(out.get("ended_web_process").is_none(), "{out}");
}

/// What the "bridge not responding" error tells an agent to do must not claim success either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn go_to_does_not_claim_success_on_a_page_that_does_not_answer() {
    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
    let h = handler(&st, &bridge);
    assert_ne!(
        call(&h, "get_diagnostics", json!({})).await.is_error,
        Some(true)
    );
    bridge.dead.store(true, Ordering::SeqCst);
    bridge.stays_dead.store(true, Ordering::SeqCst);
    let msg = err_text(
        &call(
            &h,
            "navigate",
            json!({"action": "go_to", "url": "http://tauri.localhost/index.html"}),
        )
        .await,
    );
    assert!(msg.contains("still does not answer"), "{msg}");
}

/// `WebKit` queues a navigation behind a stopped page's script, and the window reads the new URL
/// meanwhile: `ok` to an outside URL must say that it may not have left.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn go_to_outside_from_a_stopped_page_says_it_may_not_land() {
    for page_stopped in [true, false] {
        let st = state();
        let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
        let h = handler(&st, &bridge);
        assert_ne!(
            call(&h, "get_diagnostics", json!({})).await.is_error,
            Some(true)
        );
        bridge.dead.store(page_stopped, Ordering::SeqCst);
        bridge.stays_dead.store(page_stopped, Ordering::SeqCst);
        let out = ok_json(
            &call(
                &h,
                "navigate",
                json!({"action": "go_to", "url": "https://elsewhere.example/"}),
            )
            .await,
        );
        let warning = out["warning"].as_str().unwrap();
        assert!(warning.contains("outside the app"), "{warning}");
        assert_eq!(
            warning.contains("was not answering"),
            page_stopped,
            "{warning}"
        );
    }
}

/// An app whose window starts on a custom `WebviewUrl::External` is neither its dev server nor
/// Tauri's protocol, and `navigate go_to` called its own pages "outside the app".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_origin_the_bridge_answers_on_is_the_apps() {
    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
    bridge.set_window_url("main", "https://app.example.com/start?x=1");
    let h = handler(&st, &bridge);

    // The very first call, and a port is another origin.
    let own = "https://app.example.com/settings";
    let out = ok_json(&call(&h, "navigate", json!({"action": "go_to", "url": own})).await);
    assert!(out.get("warning").is_none(), "{out}");
    let other_port = "https://app.example.com:8443/";
    let out = ok_json(
        &call(
            &h,
            "navigate",
            json!({"action": "go_to", "url": other_port}),
        )
        .await,
    );
    assert!(out.get("warning").is_some(), "{out}");
    let outside = "https://elsewhere.example/";
    let out = ok_json(&call(&h, "navigate", json!({"action": "go_to", "url": outside})).await);
    assert!(
        out["warning"]
            .as_str()
            .unwrap()
            .contains(outside.trim_end_matches('/')),
        "{out}"
    );

    // And it is somewhere `go_back` can return to.
    bridge.dead.store(true, Ordering::SeqCst);
    bridge.set_window_url("main", outside);
    let out = ok_json(&call(&h, "navigate", json!({"action": "go_back"})).await);
    assert_eq!(out["recovered"], true, "{out}");
    assert_eq!(out["url"], "https://app.example.com/start?x=1", "{out}");
}

/// A window that never answered proves nothing about the page it shows, and a page with no origin
/// of its own (`about:blank`) makes nothing inside the app.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_origin_no_bridge_answered_on_stays_outside() {
    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
    bridge.set_window_url("main", "https://never.example/");
    bridge.dead.store(true, Ordering::SeqCst);
    let h = handler(&st, &bridge);
    let url = "https://never.example/x";
    let out = ok_json(&call(&h, "navigate", json!({"action": "go_to", "url": url})).await);
    assert!(out.get("warning").is_some(), "{out}");

    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
    bridge.set_window_url("main", "about:blank");
    let h = handler(&st, &bridge);
    let url = "https://elsewhere.example/";
    let out = ok_json(&call(&h, "navigate", json!({"action": "go_to", "url": url})).await);
    assert!(out.get("warning").is_some(), "{out}");
}

/// A window that never answered has no bridge at all: `go_back` must not invent a navigation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn go_back_on_a_window_that_never_answered_reports_the_failure() {
    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
    bridge.dead.store(true, Ordering::SeqCst);
    let h = handler(&st, &bridge);
    let msg = err_text(&call(&h, "navigate", json!({"action": "go_back"})).await);
    assert!(msg.contains("bridge not responding"), "{msg}");
    assert!(bridge.native_calls().is_empty());
}

/// macOS and Linux serve the app from `tauri://localhost`; refusing it stranded those platforms.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn go_to_accepts_the_apps_own_custom_scheme_but_no_other() {
    let st = state();
    let bridge = AppBridge::answering(&st, "null");
    let h = handler(&st, &bridge);
    let r = call(
        &h,
        "navigate",
        json!({"action": "go_to", "url": "tauri://localhost/index.html"}),
    )
    .await;
    assert_ne!(r.is_error, Some(true), "{}", text(&r));
    for refused in ["tauri://evil/x", "javascript:alert(1)", "data:text/html,x"] {
        let msg = err_text(&call(&h, "navigate", json!({"action": "go_to", "url": refused})).await);
        assert!(msg.contains("not allowed"), "{refused}: {msg}");
    }
    assert_eq!(bridge.native_calls().len(), 1);
}

// ── A hung page is not a missing capability ───────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_that_answered_before_is_not_diagnosed_as_a_missing_capability() {
    let st = state();
    let bridge = AppBridge::answering(&st, "1");
    let h = handler(&st, &bridge);
    assert_ne!(
        call(&h, "eval_js", json!({"code": "1"})).await.is_error,
        Some(true)
    );

    bridge.dead.store(true, Ordering::SeqCst);
    let msg = err_text(&call(&h, "eval_js", json!({"code": "1"})).await);
    assert!(msg.contains("window 'main'"), "the real label: {msg}");
    assert!(!msg.contains("capability"), "{msg}");
    assert!(msg.contains("not responding"), "{msg}");

    let report = ok_json(&call(&h, "window", json!({"action": "introspectability"})).await);
    let main = report["windows"]
        .as_array()
        .and_then(|w| w.iter().find(|e| e["label"] == "main"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(main["introspectable"], false);
    let note = main["note"].as_str().unwrap_or_default();
    assert!(!note.contains("capability"), "{note}");
    assert!(!note.contains("rebuild"), "{note}");
}

/// The advice stays for the case it is right about.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_window_that_never_answered_still_gets_the_capability_advice() {
    let st = state();
    let bridge = AppBridge::answering(&st, "1");
    bridge.dead.store(true, Ordering::SeqCst);
    let h = handler(&st, &bridge);
    let msg = err_text(&call(&h, "eval_js", json!({"code": "1"})).await);
    assert!(msg.contains("capability"), "{msg}");
    let report = ok_json(&call(&h, "window", json!({"action": "introspectability"})).await);
    assert!(
        report["windows"][0]["note"]
            .as_str()
            .is_some_and(|n| n.contains("victauri:default"))
    );
}

// ── Unreadable data is not "nothing happened" ─────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explain_on_a_page_it_cannot_read_is_an_error_not_no_errors() {
    for action in ["summary", "last_action", "diff"] {
        let st = state();
        let bridge = AppBridge::answering(&st, "[]");
        bridge.dead.store(true, Ordering::SeqCst);
        let h = handler(&st, &bridge);
        let msg = err_text(&call(&h, "explain", json!({"action": action})).await);
        assert!(msg.contains("bridge not responding"), "{action}: {msg}");
    }
}

/// Eval reaches a page whose bridge is missing, so `?.` answers `undefined`: nothing happened, and
/// no tool may report that as done. A method that really returns nothing stays a success.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_without_the_bridge_is_an_error_for_every_bridge_call() {
    let calls = [
        ("interact", json!({"action": "click", "ref_id": "k3f9e1"})),
        (
            "input",
            json!({"action": "fill", "ref_id": "k3f9e1", "value": "x"}),
        ),
        ("input", json!({"action": "press_key", "key": "Enter"})),
        ("inspect", json!({"action": "audit_accessibility"})),
        (
            "storage",
            json!({"action": "get", "storage_type": "local", "key": "k"}),
        ),
        (
            "wait_for",
            json!({"condition": "text", "value": "x", "timeout_ms": 50}),
        ),
        ("find_elements", json!({"text": "x"})),
        ("dom_snapshot", json!({})),
    ];
    for (tool, args) in calls {
        let st = state();
        let bridge = AppBridge::new(
            &st,
            Arc::new(|script: &str| {
                Some(if script.contains("!!window.__VICTAURI__") {
                    "false".to_string()
                } else {
                    "undefined".to_string()
                })
            }),
        );
        let h = handler(&st, &bridge);
        let msg = err_text(&call(&h, tool, args.clone()).await);
        assert!(msg.contains("no Victauri bridge"), "{tool} {args}: {msg}");
    }
    let st = state();
    let bridge = AppBridge::new(
        &st,
        Arc::new(|script: &str| {
            Some(if script.contains("!!window.__VICTAURI__") {
                "true".to_string()
            } else {
                "undefined".to_string()
            })
        }),
    );
    let h = handler(&st, &bridge);
    let r = call(&h, "inspect", json!({"action": "clear_highlights"})).await;
    assert_ne!(r.is_error, Some(true), "{}", text(&r));
}

/// A page with no bridge cannot run `history.back()`, so `go_back` recovers natively rather than
/// report that back ran.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn go_back_on_a_page_without_the_bridge_recovers() {
    let st = state();
    let bridge = AppBridge::new(
        &st,
        Arc::new(|script: &str| {
            Some(if script.contains("!!window.__VICTAURI__") {
                "false".to_string()
            } else {
                "undefined".to_string()
            })
        }),
    );
    let h = handler(&st, &bridge);
    let out = ok_json(&call(&h, "navigate", json!({"action": "go_back"})).await);
    assert_eq!(out["recovered"], true, "{out}");
}

/// `window.__VICTAURI__?.…` on a page without the bridge answers `undefined`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explain_on_a_page_without_the_bridge_is_an_error() {
    let st = state();
    let bridge = AppBridge::answering(&st, "null");
    let h = handler(&st, &bridge);
    let msg = err_text(&call(&h, "explain", json!({"action": "summary"})).await);
    assert!(msg.contains("event stream"), "{msg}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explain_with_a_readable_empty_stream_still_says_no_errors() {
    let st = state();
    let bridge = AppBridge::answering(&st, "[]");
    let h = handler(&st, &bridge);
    let out = ok_json(&call(&h, "explain", json!({"action": "summary"})).await);
    assert!(
        out["narrative"]
            .as_str()
            .unwrap_or_default()
            .contains("No errors")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coverage_on_a_page_it_cannot_read_is_an_error_not_zero_calls() {
    let st = state();
    let bridge = AppBridge::answering(&st, "[]");
    bridge.dead.store(true, Ordering::SeqCst);
    let h = handler(&st, &bridge);
    let msg = err_text(&call(&h, "introspect", json!({"action": "coverage"})).await);
    assert!(msg.contains("IPC log"), "{msg}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_timings_says_when_the_live_traffic_could_not_be_read() {
    let st = state();
    let bridge = AppBridge::answering(&st, "[]");
    bridge.dead.store(true, Ordering::SeqCst);
    let h = handler(&st, &bridge);
    let out = ok_json(&call(&h, "introspect", json!({"action": "command_timings"})).await);
    assert!(out["ipc_commands_observed"].is_null(), "{out}");
    assert!(out["ipc_traffic"].is_null(), "{out}");
    assert!(out["ipc_read_error"].as_str().is_some(), "{out}");
}

// ── Inputs refused instead of quietly doing something else ────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlong_app_paths_are_refused_before_the_filesystem() {
    let st = state();
    let bridge = AppBridge::answering(&st, "null");
    let h = handler(&st, &bridge);
    let path = "a/".repeat(100_000);
    let started = Instant::now();
    for (tool, args) in [
        ("read_app_file", json!({"path": path})),
        ("list_app_dir", json!({"path": path})),
    ] {
        let msg = err_text(&call(&h, tool, args).await);
        assert!(msg.contains("too long"), "{tool}: {msg}");
    }
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_for_without_a_value_fails_at_once_instead_of_polling_to_the_timeout() {
    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":false,"error":"timeout"}"#);
    let h = handler(&st, &bridge);
    for condition in ["text", "text_gone", "selector", "selector_gone", "url"] {
        for args in [
            json!({"condition": condition, "timeout_ms": 50}),
            json!({"condition": condition, "value": "", "timeout_ms": 50}),
        ] {
            let msg = err_text(&call(&h, "wait_for", args).await);
            assert!(msg.contains("missing required parameter 'value'"), "{msg}");
        }
    }
    assert_eq!(bridge.evals_containing("waitFor"), 0);
    // These conditions take no value.
    for condition in ["ipc_idle", "network_idle"] {
        let r = call(
            &h,
            "wait_for",
            json!({"condition": condition, "timeout_ms": 50}),
        )
        .await;
        assert_ne!(r.is_error, Some(true), "{}", text(&r));
    }
}

/// `wait_for expected` and `assert_semantic equals` must agree on what equal means.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_for_expected_compares_like_assert_semantic() {
    let st = state();
    let bridge = AppBridge::answering(&st, "5");
    let h = handler(&st, &bridge);
    for expected in [json!(5), json!(5.0), json!("5")] {
        let out = ok_json(
            &call(
                &h,
                "wait_for",
                json!({"condition": "expression", "value": "5", "expected": expected, "timeout_ms": 100}),
            )
            .await,
        );
        assert_eq!(out["ok"], true, "{expected}: {out}");
    }
    let bridge = AppBridge::answering(&st, "null");
    let h = handler(&st, &bridge);
    let r = call(
        &h,
        "wait_for",
        json!({"condition": "expression", "value": "null", "expected": "null", "timeout_ms": 100}),
    )
    .await;
    assert!(
        r.is_error == Some(true) || ok_json(&r)["ok"] != true,
        "null must not equal \"null\": {}",
        text(&r)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_null_is_a_value_to_compare_with() {
    let st = state();
    let bridge = AppBridge::answering(&st, "null");
    let h = handler(&st, &bridge);
    let out = ok_json(
        &call(
            &h,
            "wait_for",
            json!({"condition": "expression", "value": "null", "expected": null, "timeout_ms": 100}),
        )
        .await,
    );
    assert_eq!(out["ok"], true, "{out}");
    let out = ok_json(
        &call(
            &h,
            "verify_state",
            json!({"frontend_expr": "null", "backend_state": null}),
        )
        .await,
    );
    assert_eq!(out["passed"], true, "{out}");
    // Absent stays absent.
    let msg = err_text(&call(&h, "verify_state", json!({"frontend_expr": "null"})).await);
    assert!(
        msg.contains("either backend_state or backend_command"),
        "{msg}"
    );
    let out = ok_json(
        &call(
            &h,
            "wait_for",
            json!({"condition": "expression", "value": "null", "timeout_ms": 50}),
        )
        .await,
    );
    assert_eq!(out["ok"], false, "null is not truthy: {out}");
}

#[test]
fn list_app_dir_globs_match_as_documented() {
    let m = |n: &str, p: &str| VictauriMcpHandler::matches_glob(n, p);
    assert!(m("demo.db", "*.db"));
    assert!(m("demo.db", "demo*.db"));
    assert!(m("demo.db", "*emo.db"));
    assert!(m("demo.db", "d?mo.db"));
    assert!(m("demo.db", "*"));
    assert!(m("demo.db", "demo.db"));
    assert!(m("a.b.c", "a*c"));
    assert!(!m("demo.db", "demo"));
    assert!(!m("demo.db", "d?.db"));
    assert!(!m("demo.db", "*.sqlite"));
    assert!(m("aaaa", "a*a*a*a"));
    assert!(!m("aaaa", "a*a*a*b"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn select_option_with_no_values_argument_is_a_missing_parameter() {
    let st = state();
    let bridge = AppBridge::answering(&st, "null");
    let h = handler(&st, &bridge);
    let msg = err_text(
        &call(
            &h,
            "interact",
            json!({"action": "select_option", "ref_id": "e1"}),
        )
        .await,
    );
    assert!(msg.contains("values"), "{msg}");
    assert_eq!(bridge.evals_containing("selectOption"), 0);
    // An explicit empty list is the request to clear a `<select multiple>`.
    let _ = call(
        &h,
        "interact",
        json!({"action": "select_option", "ref_id": "e1", "values": []}),
    )
    .await;
    assert_eq!(bridge.evals_containing("selectOption"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trace_start_on_an_unknown_window_fails_and_stop_says_nothing_was_running() {
    let st = state();
    let bridge = AppBridge::answering(&st, "null");
    let h = handler(&st, &bridge);
    let msg = err_text(
        &call(
            &h,
            "trace",
            json!({"action": "start", "webview_label": "nope"}),
        )
        .await,
    );
    assert!(msg.contains("window not found"), "{msg}");
    assert!(!st.screencast.is_active());
    let out = ok_json(&call(&h, "trace", json!({"action": "stop"})).await);
    assert_eq!(out["stopped"], false, "{out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_set_without_a_value_is_refused_but_an_explicit_null_is_stored() {
    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
    let h = handler(&st, &bridge);
    let msg = err_text(&call(&h, "storage", json!({"action": "set", "key": "nv"})).await);
    assert!(msg.contains("missing required parameter 'value'"), "{msg}");
    assert_eq!(bridge.evals_containing("setLocalStorage"), 0);
    let r = call(
        &h,
        "storage",
        json!({"action": "set", "key": "nv", "value": null}),
    )
    .await;
    assert_ne!(r.is_error, Some(true), "{}", text(&r));
    assert_eq!(bridge.evals_containing("setLocalStorage"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn window_resize_and_move_report_what_the_os_applied() {
    let st = state();
    let mut bridge = AppBridge::answering(&st, "null");
    bridge.os_size_limit = Some((1000, 700));
    let h = handler(&st, &bridge);
    let out = ok_json(
        &call(
            &h,
            "window",
            json!({"action": "resize", "width": 5000, "height": 4000}),
        )
        .await,
    );
    assert_eq!(out["width"], 1000, "{out}");
    assert_eq!(out["height"], 700, "{out}");
    assert_eq!(
        out["requested"],
        json!({"width": 5000, "height": 4000}),
        "{out}"
    );
    assert!(out.get("settled").is_none(), "{out}");
    let out = ok_json(&call(&h, "window", json!({"action": "move_to", "x": 9, "y": 8})).await);
    assert_eq!(out["requested"], json!({"x": 9, "y": 8}), "{out}");
    assert_eq!(out["x"], 9, "{out}");
    assert!(out.get("settled").is_none(), "{out}");
}

/// Linux and macOS apply a resize or move after the call returns: reading back at once reported
/// the size the window had before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn window_resize_and_move_wait_for_an_os_that_applies_them_later() {
    let st = state();
    let mut bridge = AppBridge::answering(&st, "null");
    bridge.os_apply_delay = Some(Duration::from_millis(120));
    let h = handler(&st, &bridge);
    let out = ok_json(
        &call(
            &h,
            "window",
            json!({"action": "resize", "width": 640, "height": 480}),
        )
        .await,
    );
    assert_eq!(out["width"], 640, "{out}");
    assert_eq!(out["height"], 480, "{out}");
    assert!(out.get("settled").is_none(), "{out}");
    let out = ok_json(&call(&h, "window", json!({"action": "move_to", "x": 9, "y": 8})).await);
    assert_eq!(out["x"], 9, "{out}");
    assert!(out.get("settled").is_none(), "{out}");
}

/// A window manager may ignore the request: the result says so, in bounded time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn window_resize_and_move_say_when_nothing_changed() {
    let st = state();
    let mut bridge = AppBridge::answering(&st, "null");
    bridge.os_ignores_geometry = true;
    let h = handler(&st, &bridge);
    let start = Instant::now();
    let out = ok_json(
        &call(
            &h,
            "window",
            json!({"action": "resize", "width": 640, "height": 480}),
        )
        .await,
    );
    assert_eq!(out["width"], 800, "the real size: {out}");
    assert_eq!(out["settled"], false, "{out}");
    let out = ok_json(&call(&h, "window", json!({"action": "move_to", "x": 9, "y": 8})).await);
    assert_eq!(out["x"], 0, "{out}");
    assert_eq!(out["settled"], false, "{out}");
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}

/// `wait_for` reports a timeout as `ok:false`, but a condition that can never hold is a mistake
/// in the call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_for_an_invalid_selector_is_a_tool_error_and_a_timeout_is_not() {
    let st = state();
    let bridge = AppBridge::answering(
        &st,
        r#"{"__victauri_ok":{"ok":false,"error":"invalid CSS selector: ## - bad","hint":"CHECK_INPUT","elapsed_ms":0},"__victauri_type":"value"}"#,
    );
    let h = handler(&st, &bridge);
    let msg = err_text(
        &call(
            &h,
            "wait_for",
            json!({"condition": "selector", "value": "##"}),
        )
        .await,
    );
    assert!(msg.contains("invalid CSS selector"), "{msg}");
    assert!(msg.contains("CHECK_INPUT"), "{msg}");

    let st = state();
    let bridge = AppBridge::answering(
        &st,
        r#"{"__victauri_ok":{"ok":false,"error":"timeout after 50ms","elapsed_ms":50},"__victauri_type":"value"}"#,
    );
    let h = handler(&st, &bridge);
    let out = ok_json(
        &call(
            &h,
            "wait_for",
            json!({"condition": "selector", "value": "#x", "timeout_ms": 50}),
        )
        .await,
    );
    assert_eq!(out["ok"], false, "{out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logs_bodies_false_is_passed_to_the_page_for_ipc_and_network() {
    const EMPTY: &str = r#"{"__victauri_ok":[],"__victauri_type":"value"}"#;
    for (action, method) in [("ipc", "getIpcLog"), ("network", "getNetworkLog")] {
        let st = state();
        let bridge = AppBridge::answering(&st, EMPTY);
        let h = handler(&st, &bridge);
        let _ = call(
            &h,
            "logs",
            json!({"action": action, "bodies": false, "limit": 1000}),
        )
        .await;
        assert_eq!(
            bridge.evals_containing("{ bodies: false }"),
            1,
            "{action}: {:?}",
            bridge.evals.lock().unwrap().last()
        );
        assert_eq!(bridge.evals_containing(method), 1, "{action}");

        let st = state();
        let bridge = AppBridge::answering(&st, EMPTY);
        let h = handler(&st, &bridge);
        let _ = call(&h, "logs", json!({"action": action})).await;
        assert_eq!(
            bridge.evals_containing("bodies"),
            0,
            "{action}: the default keeps them"
        );
    }
    // With wait_for_capture too.
    let st = state();
    let bridge = AppBridge::answering(&st, EMPTY);
    let h = handler(&st, &bridge);
    let _ = call(
        &h,
        "logs",
        json!({"action": "ipc", "bodies": false, "wait_for_capture": true}),
    )
    .await;
    assert_eq!(bridge.evals_containing("{ bodies: false }"), 1);
}

/// The stuck-page detour must not stay in the app's history: `go_back` would land on it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_health_detour_is_replaced_not_left_in_history() {
    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
    let landing = bridge.clone();
    let h = handler(&st, &bridge);
    assert_ne!(
        call(&h, "get_diagnostics", json!({})).await.is_error,
        Some(true)
    );
    bridge.dead.store(true, Ordering::SeqCst);
    // The cross-site detour lands, and then the page's own script replaces it.
    let port = st.port.load(Ordering::Relaxed);
    let lander = std::thread::spawn(move || {
        for _ in 0..1000 {
            if landing.evals_containing("location.replace(") > 0 {
                landing.set_window_url("main", "http://tauri.localhost/index.html");
                return;
            }
            if landing
                .native_calls()
                .iter()
                .any(|(_, c)| c.ends_with("/health"))
            {
                landing.set_window_url("main", &format!("http://127.0.0.1:{port}/health"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let out = ok_json(&call(&h, "navigate", json!({"action": "go_back"})).await);
    assert_eq!(out["recovered"], true, "{out}");
    let _ = lander.join();
    assert_eq!(
        bridge.evals_containing("location.replace(\"http://tauri.localhost/index.html\")"),
        1,
        "the detour's entry is replaced"
    );
    let calls = bridge.native_calls();
    assert_eq!(
        calls.iter().filter(|(_, c)| c.ends_with("/health")).count(),
        1,
        "{calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|(_, c)| c.starts_with("navigate http://tauri.localhost")),
        "a native navigation would push another entry: {calls:?}"
    );
}

/// A webview that refuses the script navigation (or a detour that never landed) still gets there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_detour_that_cannot_be_replaced_falls_back_to_a_native_navigation() {
    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
    let landing = bridge.clone();
    let h = handler(&st, &bridge);
    assert_ne!(
        call(&h, "get_diagnostics", json!({})).await.is_error,
        Some(true)
    );
    bridge.dead.store(true, Ordering::SeqCst);
    let port = st.port.load(Ordering::Relaxed);
    // The detour lands; the script navigation does nothing.
    let lander = std::thread::spawn(move || {
        for _ in 0..1000 {
            if landing
                .native_calls()
                .iter()
                .any(|(_, c)| c.ends_with("/health"))
            {
                landing.set_window_url("main", &format!("http://127.0.0.1:{port}/health"));
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let out = ok_json(&call(&h, "navigate", json!({"action": "go_back"})).await);
    assert_eq!(out["recovered"], true, "{out}");
    let _ = lander.join();
    assert_eq!(
        bridge.native_calls().last().map(|(_, c)| c.as_str()),
        Some("navigate http://tauri.localhost/index.html")
    );
}

/// Explicit null is a value (`present_value`): it is stored, not reported as missing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_set_stores_an_explicit_null() {
    let st = state();
    let bridge = AppBridge::answering(
        &st,
        r#"{"__victauri_ok":{"ok":true},"__victauri_type":"value"}"#,
    );
    let h = handler(&st, &bridge);
    let r = call(
        &h,
        "storage",
        json!({"action": "set", "key": "k", "value": null}),
    )
    .await;
    assert_ne!(r.is_error, Some(true), "{}", text(&r));
    assert_eq!(bridge.evals_containing(r#"setLocalStorage("k", null)"#), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn animation_failures_are_tool_errors() {
    // The bridge's real failure shapes (js_bridge.rs listAnimations / scrubPrepare / readSweep).
    for (args, body) in [
        (
            json!({"action": "list", "selector": "###"}),
            r#"{"error":"invalid selector: ###"}"#,
        ),
        (
            json!({"action": "scrub"}),
            r#"{"error":"no target: selector matched nothing and no element is animating"}"#,
        ),
        (
            json!({"action": "sample"}),
            r#"{"error":"no recorder armed — call sample with record=true first"}"#,
        ),
    ] {
        let st = state();
        let bridge = AppBridge::answering(&st, body);
        let h = handler(&st, &bridge);
        let r = call(&h, "animation", args.clone()).await;
        assert_eq!(r.is_error, Some(true), "{args}: {}", text(&r));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contract_record_refuses_args_it_would_otherwise_drop() {
    let st = state();
    let bridge = AppBridge::answering(&st, "{}");
    let h = handler(&st, &bridge);
    for args in [json!("not-an-object"), json!(42)] {
        let result = h
            .execute_tool(
                "introspect",
                json!({"action": "contract_record", "command": "c", "args": args}),
            )
            .await;
        assert!(result.is_err(), "non-object args must be refused");
    }
    assert_eq!(bridge.evals_containing("invoke("), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_flat_argument_error_points_at_args() {
    let st = state();
    let bridge = AppBridge::new(
        &st,
        Arc::new(|_| {
            Some(r#"{"__victauri_err":"invalid args `name` for command `greet`: command greet missing required key name"}"#.to_string())
        }),
    );
    let h = handler(&st, &bridge);
    let msg = err_text(
        &call(
            &h,
            "invoke_command",
            json!({"command": "greet", "name": "x"}),
        )
        .await,
    );
    assert!(msg.contains("nested under `args`"), "{msg}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn type_text_refuses_what_would_wedge_the_page() {
    let st = state();
    let bridge = AppBridge::answering(&st, r#"{"ok":true}"#);
    let h = handler(&st, &bridge);
    let msg = err_text(
        &call(
            &h,
            "input",
            json!({"action": "type_text", "ref_id": "e1", "text": "x".repeat(2 * 64 * 1024)}),
        )
        .await,
    );
    assert!(msg.contains("fill"), "points at the alternative: {msg}");
    assert_eq!(bridge.evals_containing(".type("), 0);
}

// ── Fault paths honour the call's documented contract ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fault_delay_counts_against_the_calls_time_budget() {
    let st = state();
    let bridge = AppBridge::answering(&st, "1");
    let h = handler(&st, &bridge);
    let _ = call(
        &h,
        "fault",
        json!({"action": "inject", "command": "slow", "fault_type": "delay", "delay_ms": 3000}),
    )
    .await;
    let started = Instant::now();
    let msg = err_text(
        &call(
            &h,
            "invoke_command",
            json!({"command": "slow", "timeout_ms": 300}),
        )
        .await,
    );
    assert!(
        started.elapsed() < Duration::from_millis(2500),
        "{:?}",
        started.elapsed()
    );
    assert!(msg.contains("FAULT INJECTED"), "{msg}");
    assert_eq!(bridge.evals_containing("invoke(\"slow\""), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fault_paths_keep_the_with_logs_envelope() {
    for (fault, args) in [
        ("drop", json!({})),
        ("corrupt", json!({})),
        ("error", json!({"error_message": "boom"})),
    ] {
        let st = state();
        let bridge = AppBridge::answering(&st, "1");
        let h = handler(&st, &bridge);
        let mut inject = json!({"action": "inject", "command": "c", "fault_type": fault});
        inject
            .as_object_mut()
            .unwrap()
            .extend(args.as_object().unwrap().clone());
        let _ = call(&h, "fault", inject).await;
        let r = call(
            &h,
            "invoke_command",
            json!({"command": "c", "with_logs": true}),
        )
        .await;
        let out: serde_json::Value = serde_json::from_str(&text(&r))
            .unwrap_or_else(|e| panic!("{fault}: not JSON ({e}): {}", text(&r)));
        assert!(
            out.get("backend_logs").is_some() || out.get("backend_log_capture").is_some(),
            "{fault}: {out}"
        );
    }
}

// ── Protocol ──────────────────────────────────────────────────────────────────────────────

fn app(state: &Arc<VictauriState>, bridge: &AppBridge) -> axum::Router {
    super::build_app_full(Arc::clone(state), Arc::new(bridge.clone()), None, None)
}

async fn post(app: &axum::Router, path: &str, body: String) -> (axum::http::StatusCode, Vec<u8>) {
    let req = axum::extract::Request::builder()
        .method("POST")
        .uri(path)
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(axum::body::Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, bytes.to_vec())
}

async fn mcp_rpc(app: &axum::Router, method: &str, params: serde_json::Value) -> serde_json::Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let (status, bytes) = post(app, "/mcp", body).await;
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    assert!(status.is_success(), "{method}: {status} {raw}");
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{method}: {e}: {raw}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_identifies_itself_and_resources_agree_on_their_mime_type() {
    let st = state();
    let bridge = AppBridge::answering(&st, "[]");
    let app = app(&st, &bridge);
    let init = mcp_rpc(
        &app,
        "initialize",
        json!({"protocolVersion": "2025-03-26", "capabilities": {},
               "clientInfo": {"name": "t", "version": "0"}}),
    )
    .await;
    assert_eq!(init["result"]["serverInfo"]["name"], "victauri", "{init}");
    assert_eq!(
        init["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION"),
        "{init}"
    );
    for uri in [
        RESOURCE_URI_STATE,
        RESOURCE_URI_WINDOWS,
        RESOURCE_URI_IPC_LOG,
    ] {
        let read = mcp_rpc(&app, "resources/read", json!({"uri": uri})).await;
        assert_eq!(
            read["result"]["contents"][0]["mimeType"], "application/json",
            "{uri}: {read}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rest_tool_list_carries_input_schemas() {
    let st = state();
    let bridge = AppBridge::answering(&st, "[]");
    let app = app(&st, &bridge);
    let req = axum::extract::Request::builder()
        .uri("/api/tools")
        .header("host", "127.0.0.1")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let tools: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
    let eval = tools.iter().find(|t| t["name"] == "eval_js").unwrap();
    assert!(
        eval["input_schema"]["properties"]["code"].is_object(),
        "{eval}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_body_gets_a_json_error() {
    let st = state();
    let bridge = AppBridge::answering(&st, "[]");
    let app = app(&st, &bridge);
    let huge = format!(r#"{{"code":"{}"}}"#, "x".repeat(3 * 1024 * 1024));
    let (status, bytes) = post(&app, "/api/tools/eval_js", huge).await;
    assert_eq!(status, axum::http::StatusCode::PAYLOAD_TOO_LARGE);
    let body: serde_json::Value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&bytes)));
    assert!(
        body["error"].as_str().is_some_and(|e| e.contains("limit")),
        "{body}"
    );
}

/// An export has no size limit, so a real session could not be imported again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_exported_session_round_trips_over_rest() {
    let st = state();
    let bridge = AppBridge::answering(&st, "[]");
    let app = app(&st, &bridge);
    st.recorder.start_session("big".to_string()).unwrap();
    for i in 0..1_000u32 {
        st.recorder.record_event(victauri_core::AppEvent::console(
            "log",
            format!("console line {i} {}", "x".repeat(4_000)),
            chrono::Utc::now(),
        ));
    }
    let session = st.recorder.stop().unwrap();
    let exported = serde_json::to_string_pretty(&session).unwrap();
    assert!(exported.len() > 3 * 1024 * 1024);
    let body = json!({"action": "import", "session_json": exported}).to_string();
    let (status, bytes) = post(&app, "/api/tools/recording", body).await;
    let raw = String::from_utf8_lossy(&bytes[..bytes.len().min(400)]).into_owned();
    assert!(status.is_success(), "{status}: {raw}");
    let out: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(out["result"]["imported"], true, "{raw}");
    assert_eq!(out["result"]["event_count"], 1_000, "{raw}");
}

// ── Privacy ───────────────────────────────────────────────────────────────────────────────

fn event_bus_with_payload(state: &Arc<VictauriState>) {
    state
        .event_bus
        .push(crate::introspection::CapturedTauriEvent {
            name: "license-updated".to_string(),
            payload: r#"{"customer":"acme","seats":12}"#.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        });
}

const WAIT_EVENT: fn() -> serde_json::Value = || json!({"condition": "event", "value": "license-updated", "timeout_ms": 100, "since_ms": 60000});

/// `introspect event_bus` is refused under these configurations; `wait_for` must not hand out
/// the same payload. The wait itself keeps working (it is how async work is awaited).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_for_event_omits_the_payload_where_the_event_bus_is_not_readable() {
    let disabled = |name: &str| {
        let mut p = crate::privacy::PrivacyConfig::default();
        p.disabled_tools.insert(name.to_string());
        p
    };
    for privacy in [
        crate::privacy::observe_privacy_config(),
        crate::privacy::test_privacy_config(),
        disabled("introspect.event_bus"),
        disabled("introspect"),
    ] {
        let st = state_with(privacy);
        event_bus_with_payload(&st);
        let bridge = AppBridge::answering(&st, "null");
        let h = handler(&st, &bridge);
        let out = ok_json(&call(&h, "wait_for", WAIT_EVENT()).await);
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["event"]["name"], "license-updated", "{out}");
        assert!(out["event"].get("payload").is_none(), "{out}");
        assert!(!text(&call(&h, "wait_for", WAIT_EVENT()).await).contains("acme"));
    }
    let st = state();
    event_bus_with_payload(&st);
    let bridge = AppBridge::answering(&st, "null");
    let h = handler(&st, &bridge);
    let out = ok_json(&call(&h, "wait_for", WAIT_EVENT()).await);
    assert!(
        out["event"]["payload"]
            .as_str()
            .is_some_and(|p| p.contains("acme")),
        "{out}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_disable_names_block_their_actions() {
    for (name, tool, args) in [
        ("get_storage", "storage", json!({"action": "get"})),
        ("get_cookies", "storage", json!({"action": "get_cookies"})),
        ("get_window_state", "window", json!({"action": "get_state"})),
        ("list_windows", "window", json!({"action": "list"})),
    ] {
        let mut privacy = crate::privacy::PrivacyConfig::default();
        privacy.disabled_tools.insert(name.to_string());
        let st = state_with(privacy);
        let bridge = AppBridge::answering(&st, "[]");
        let h = handler(&st, &bridge);
        let msg = err_text(&call(&h, tool, args).await);
        assert!(msg.contains("disabled"), "{name}: {msg}");
        assert_eq!(
            bridge.evals.lock().unwrap().len(),
            0,
            "{name} reached the page"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invoke_command_under_test_runs_only_what_the_allowlist_names() {
    let mut privacy = crate::privacy::test_privacy_config();
    privacy.command_allowlist = Some(std::iter::once("greet".to_string()).collect());
    let st = state_with(privacy);
    let bridge = AppBridge::answering(&st, "\"hi\"");
    let h = handler(&st, &bridge);
    assert!(
        h.is_tool_enabled("invoke_command"),
        "must surface in the tool lists"
    );

    let r = call(&h, "invoke_command", json!({"command": "greet"})).await;
    assert_ne!(r.is_error, Some(true), "{}", text(&r));
    assert_eq!(bridge.evals_containing("invoke(\"greet\""), 1);

    let r = call(&h, "invoke_command", json!({"command": "other"})).await;
    assert_eq!(r.is_error, Some(true), "{}", text(&r));
    assert_eq!(bridge.evals_containing("invoke(\"other\""), 0);

    // Without an allowlist the Test profile keeps it closed.
    let st = state_with(crate::privacy::test_privacy_config());
    let bridge = AppBridge::answering(&st, "\"hi\"");
    let h = handler(&st, &bridge);
    assert!(!h.is_tool_enabled("invoke_command"));
    let r = call(&h, "invoke_command", json!({"command": "greet"})).await;
    assert_eq!(r.is_error, Some(true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_reads_of_a_sensitive_key_are_redacted() {
    let st = state_with(crate::privacy::test_privacy_config());
    let bridge = AppBridge::new(
        &st,
        Arc::new(|script: &str| {
            if script.contains("session_token") {
                Some("\"abc123opaque\"".to_string())
            } else if script.contains("getCookies") {
                Some(r#"[{"name":"session_token","value":"abc123opaque"},{"name":"theme","value":"dark"}]"#.to_string())
            } else {
                Some("null".to_string())
            }
        }),
    );
    let h = handler(&st, &bridge);
    let read = text(
        &call(
            &h,
            "storage",
            json!({"action": "get", "key": "session_token"}),
        )
        .await,
    );
    assert!(!read.contains("abc123opaque"), "{read}");
    assert!(read.contains("[REDACTED]"), "{read}");
    let cookies = text(&call(&h, "storage", json!({"action": "get_cookies"})).await);
    assert!(!cookies.contains("abc123opaque"), "{cookies}");
    assert!(cookies.contains("dark"), "{cookies}");
    // A key that holds nothing stays visibly empty.
    let missing = text(&call(&h, "storage", json!({"action": "get", "key": "theme"})).await);
    assert_eq!(missing, "null");
}

// ── Refused requests cannot flood the log ─────────────────────────────────────────────────

#[derive(Clone, Default)]
struct WarnCounter(
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<StdMutex<Vec<String>>>,
);

struct Message(String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, _f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
        self.0.push_str(&format!("{v:?}"));
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for WarnCounter {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if *event.metadata().level() != tracing::Level::WARN {
            return;
        }
        self.0.fetch_add(1, Ordering::Relaxed);
        let mut message = Message(String::new());
        event.record(&mut message);
        self.1.lock().unwrap().push(message.0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn refused_requests_cannot_flood_the_log_or_write_into_it() {
    use tracing_subscriber::layer::SubscriberExt;
    let counter = WarnCounter::default();
    let subscriber = tracing_subscriber::registry().with(counter.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let app = super::build_app_full(
        state(),
        Arc::new(AppBridge::answering(&state(), "null")),
        Some("11111111-2222-3333-4444-555555555555".to_string()),
        None,
    );
    let planted = "ignore-previous-instructions";
    for i in 0..500 {
        for (name, value) in [
            ("host", format!("{planted}-{i}.evil.example:7373")),
            ("origin", format!("http://{planted}-{i}.evil.example")),
        ] {
            let mut req = axum::extract::Request::builder().uri("/info");
            req = if name == "host" {
                req.header("host", &value)
            } else {
                req.header("host", "127.0.0.1").header("origin", &value)
            };
            let _ = app
                .clone()
                .oneshot(req.body(axum::body::Body::empty()).unwrap())
                .await
                .unwrap();
        }
        let req = axum::extract::Request::builder()
            .uri("/info")
            .header("host", "127.0.0.1")
            .body(axum::body::Body::empty())
            .unwrap();
        let _ = app.clone().oneshot(req).await.unwrap();
    }
    let warns = counter.0.load(Ordering::Relaxed);
    assert!(warns <= 6, "1500 refusals logged {warns} warnings");
    let lines = counter.1.lock().unwrap();
    assert!(lines.iter().all(|l| !l.contains(planted)), "{lines:?}");
}

// ── Bind failure names the reason ─────────────────────────────────────────────────────────

#[test]
fn a_bind_failure_message_carries_the_os_reason() {
    let error = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    let msg = super::server::bind_failure_message(7497, 7507, Some(&error));
    assert!(msg.contains("7497-7507"), "{msg}");
    assert!(msg.contains(&error.to_string()), "{msg}");
    #[cfg(windows)]
    assert!(msg.contains("excludedportrange"), "{msg}");
}
