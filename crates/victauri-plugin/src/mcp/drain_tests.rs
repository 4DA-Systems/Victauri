//! Recording drain + replay regressions (audit C5/V-10, C6, V-3, V-4).
//!
//! `PageBridge` answers the drain's `drainEvents(afterSeq, instance, floor)` read the way the
//! JS bridge does — every entry with a sequence number above `afterSeq` (all of them for another
//! page instance) — so these tests pin the Rust side of the protocol; the JS side is covered by
//! `tests/bridge_tests.rs::recording_drain_reads_each_event_exactly_once`.

use std::sync::Mutex as StdMutex;

use serde_json::json;
use victauri_core::{AppEvent, IpcCall, IpcResult, WindowState};

use super::*;

const INSTANCE: &str = "page-instance-1";

#[derive(Clone, Default)]
struct PageBridge {
    pending: Option<crate::PendingCallbacks>,
    /// `(seq, raw event JSON)` — raw so a test can plant text `serde_json` rejects.
    entries: Arc<StdMutex<Vec<(u64, String)>>>,
    delay_ms: u64,
    /// `(label, script)` of every eval.
    evals: Arc<StdMutex<Vec<(Option<String>, String)>>>,
}

impl PageBridge {
    fn new(state: &Arc<VictauriState>) -> Self {
        Self {
            pending: Some(state.pending_evals.clone()),
            ..Self::default()
        }
    }

    fn push(&self, raw_event: &str) {
        let mut e = self.entries.lock().unwrap();
        let seq = e.len() as u64 + 1;
        e.push((seq, raw_event.to_string()));
    }

    fn answer(&self, id: String, body: String) {
        let Some(pending) = self.pending.clone() else {
            return;
        };
        let delay = self.delay_ms;
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(delay));
            if let Some(tx) = pending.blocking_lock().remove(&id) {
                let _ = tx.send(body);
            }
        });
    }

    /// Labels that received an eval invoking `command`.
    fn invoked_in(&self, command: &str) -> Vec<Option<String>> {
        let needle = format!("invoke({}", js_string(command));
        self.evals
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| s.contains(&needle))
            .map(|(l, _)| l.clone())
            .collect()
    }

    fn drain_reads(&self) -> usize {
        self.evals
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| s.contains("drainEvents("))
            .count()
    }
}

/// The quoted id after `marker` (`marker"<uuid>"`).
fn quoted_id_after(script: &str, marker: &str) -> Option<String> {
    let start = script.find(marker)? + marker.len();
    script.get(start..start + 36).map(str::to_string)
}

impl WebviewBridge for PageBridge {
    fn eval_webview(&self, label: Option<&str>, script: &str) -> Result<(), String> {
        self.evals
            .lock()
            .unwrap()
            .push((label.map(str::to_string), script.to_string()));
        if script.contains("probe_ok") {
            if let Some(id) = quoted_id_after(script, "id:\"") {
                self.answer(id, "\"probe_ok\"".to_string());
            }
        } else if let Some(args_at) = script.find("drainEvents(") {
            let args = &script[args_at + "drainEvents(".len()..];
            let mut parts = args.splitn(3, ',');
            let after: u64 = parts.next().unwrap().trim().parse().unwrap();
            let instance = parts.next().unwrap().trim();
            let after = if instance == js_string(INSTANCE) {
                after
            } else {
                0
            };
            let entries = self.entries.lock().unwrap();
            let events: Vec<&str> = entries
                .iter()
                .filter(|(seq, _)| *seq > after)
                .map(|(_, raw)| raw.as_str())
                .collect();
            let body = format!(
                r#"{{"instance":{},"seq":{},"events":[{}]}}"#,
                js_string(INSTANCE),
                entries.len(),
                events.join(",")
            );
            if let Some(id) = quoted_id_after(script, "_evalSettle(\"") {
                self.answer(id, body);
            }
        }
        Ok(())
    }
    fn get_window_states(&self, _l: Option<&str>) -> Vec<WindowState> {
        Vec::new()
    }
    fn list_window_labels(&self) -> Vec<String> {
        vec!["main".to_string(), "popup".to_string()]
    }
    fn get_native_handle(&self, _l: Option<&str>) -> Result<isize, String> {
        Err("no handle".to_string())
    }
    fn manage_window(&self, _l: Option<&str>, _a: &str) -> Result<String, String> {
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
}

fn state() -> Arc<VictauriState> {
    let mut s = VictauriState::for_tests();
    s.eval_timeout = std::time::Duration::from_millis(100);
    Arc::new(s)
}

async fn call(h: &VictauriMcpHandler, tool: &str, args: serde_json::Value) -> CallToolResult {
    match h.execute_tool(tool, args).await {
        Ok(r) => r,
        Err(_) => panic!("dispatch returned a transport error"),
    }
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

fn console_json(message: &str, timestamp_ms: i64) -> String {
    json!({"type": "console", "level": "log", "message": message, "timestamp": timestamp_ms})
        .to_string()
}

async fn drain(state: &Arc<VictauriState>, bridge: &Arc<dyn WebviewBridge>) -> Option<usize> {
    crate::mcp::server::drain_window_into_recording(state, bridge, "main").await
}

// C5 / V-10: an event stamped ahead of the Rust clock was re-read on every drain (the
// clamp-to-now watermark never got past it): one event, recorded 5 times in 5 drains.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_dated_event_is_recorded_once() {
    let state = state();
    let page = PageBridge::new(&state);
    let bridge: Arc<dyn WebviewBridge> = Arc::new(page.clone());
    let h = VictauriMcpHandler::new(state.clone(), Arc::clone(&bridge));
    let _ = call(&h, "recording", json!({"action": "start"})).await;
    let future_ms = chrono::Utc::now().timestamp_millis() + 3_600_000;
    page.push(&console_json("from the future", future_ms));
    for _ in 0..5 {
        assert!(drain(&state, &bridge).await.is_some());
    }
    assert_eq!(state.recorder.event_count(), 1);
    // Later events still flow.
    page.push(&console_json("now", chrono::Utc::now().timestamp_millis()));
    assert_eq!(drain(&state, &bridge).await, Some(1));
    assert_eq!(state.recorder.event_count(), 2);
}

// V-4: a lone surrogate in page JSON made serde_json reject the whole reply, so the drain
// returned None without advancing — one `console.log('\ud800')` stalled the window's recording.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lone_surrogate_does_not_stall_the_drain() {
    let state = state();
    let page = PageBridge::new(&state);
    let bridge: Arc<dyn WebviewBridge> = Arc::new(page.clone());
    let h = VictauriMcpHandler::new(state.clone(), Arc::clone(&bridge));
    let _ = call(&h, "recording", json!({"action": "start"})).await;
    let now = chrono::Utc::now().timestamp_millis();
    page.push(&format!(
        r#"{{"type":"console","level":"log","message":"\ud800 hi","timestamp":{now}}}"#
    ));
    assert_eq!(drain(&state, &bridge).await, Some(1));
    assert_eq!(drain(&state, &bridge).await, Some(0), "read exactly once");
    page.push(&console_json("next", now));
    assert_eq!(
        drain(&state, &bridge).await,
        Some(1),
        "the window keeps draining"
    );
    let session = state.recorder.export().unwrap();
    let AppEvent::Console { message, .. } = &session.events[0].event else {
        panic!("expected a console event");
    };
    assert_eq!(message, "\u{FFFD} hi");
}

// V-4: the eval envelope itself — a lone surrogate sent it down the raw-string fallback, and
// every consumer's own parse of that failed too (explain / introspect came back empty).
#[test]
fn eval_envelope_with_a_lone_surrogate_parses() {
    let raw = r#"{"__victauri_ok":["a\ud800"],"__victauri_type":"object"}"#.to_string();
    let value = unwrap_eval_envelope(raw).unwrap();
    let names: Vec<String> = serde_json::from_str(&value).unwrap();
    assert_eq!(names, vec!["a\u{FFFD}".to_string()]);
}

// C6: a drain in flight across `recording stop` + `start` recorded the OLD recording's events
// into the NEW one (the auditors' PoC: 1 leaked event).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_flight_drain_does_not_leak_into_the_next_recording() {
    let state = state();
    let page = PageBridge {
        delay_ms: 300,
        ..PageBridge::new(&state)
    };
    let bridge: Arc<dyn WebviewBridge> = Arc::new(page.clone());
    let h = VictauriMcpHandler::new(state.clone(), Arc::clone(&bridge));
    let _ = call(
        &h,
        "recording",
        json!({"action": "start", "session_id": "A"}),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    page.push(&console_json(
        "during A",
        chrono::Utc::now().timestamp_millis(),
    ));
    let st = state.clone();
    let b = Arc::clone(&bridge);
    let in_flight = tokio::spawn(async move { drain(&st, &b).await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let _ = call(&h, "recording", json!({"action": "stop"})).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let _ = call(
        &h,
        "recording",
        json!({"action": "start", "session_id": "B"}),
    )
    .await;
    let _ = in_flight.await;
    let session = state.recorder.export().unwrap();
    assert_eq!(session.id, "B");
    assert!(
        session.events.is_empty(),
        "recording B holds events from before it started: {:?}",
        session.events
    );
}

// C6: `start` is visible before the drain epoch is reset; a drain in that gap must not read
// with the previous recording's positions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drain_waits_for_the_new_recordings_epoch() {
    let state = state();
    let page = PageBridge::new(&state);
    let bridge: Arc<dyn WebviewBridge> = Arc::new(page.clone());
    let _ = state
        .recorder
        .start_session("no-reset-yet".to_string())
        .unwrap();
    assert_eq!(drain(&state, &bridge).await, Some(0));
    assert_eq!(page.drain_reads(), 0, "no read before the epoch is reset");
}

// V-3: a route-mocked IPC call (a page can `addRoute` + `fetch` + `clearRoutes` through the
// frozen bridge API) was recorded as a real successful call, and replay then invoked it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mocked_ipc_is_recorded_as_mocked_and_never_replayed() {
    let state = state();
    let page = PageBridge::new(&state);
    let bridge: Arc<dyn WebviewBridge> = Arc::new(page.clone());
    let h = VictauriMcpHandler::new(state.clone(), Arc::clone(&bridge));
    let _ = call(&h, "recording", json!({"action": "start"})).await;
    let now = chrono::Utc::now().timestamp_millis();
    page.push(
        &json!({"type": "ipc", "command": "quit_app", "status": "ok", "mocked": true,
                "arg_size_bytes": 0, "timestamp": now, "seq_ts": now})
        .to_string(),
    );
    assert_eq!(drain(&state, &bridge).await, Some(1));
    let calls = state.recorder.ipc_replay_sequence();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].mocked, "the mocked flag must survive the drain");

    let r = call(&h, "recording", json!({"action": "replay"})).await;
    let body = text(&r);
    assert!(
        page.invoked_in("quit_app").is_empty(),
        "a mocked call was replayed for real: {body}"
    );
    assert!(body.contains("route rule"), "{body}");
}

fn ipc_in(command: &str, label: &str) -> AppEvent {
    AppEvent::Ipc(IpcCall::new(
        format!("c-{command}"),
        command,
        chrono::Utc::now(),
        IpcResult::Ok(serde_json::Value::Null),
        Some(1),
        0,
        label,
    ))
}

// V-3: replay ran every recorded call in the window named by the replay's own
// `webview_label` (default: main) — so a call recorded in a low-privilege window ran with
// main's capabilities.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_runs_each_call_in_the_window_that_recorded_it() {
    let state = state();
    let page = PageBridge::new(&state);
    let bridge: Arc<dyn WebviewBridge> = Arc::new(page.clone());
    let h = VictauriMcpHandler::new(state.clone(), Arc::clone(&bridge));
    state.recorder.start("s".to_string()).unwrap();
    state.recorder.record_event(ipc_in("popup_cmd", "popup"));
    state.recorder.record_event(ipc_in("main_cmd", "main"));

    let _ = call(&h, "recording", json!({"action": "replay"})).await;
    assert_eq!(
        page.invoked_in("popup_cmd"),
        vec![Some("popup".to_string())],
        "a call recorded in 'popup' must run only in 'popup'"
    );
    assert_eq!(page.invoked_in("main_cmd"), vec![Some("main".to_string())]);

    // `webview_label` limits replay to that window's calls; it never redirects them.
    let r = call(
        &h,
        "recording",
        json!({"action": "replay", "webview_label": "main"}),
    )
    .await;
    assert_eq!(
        page.invoked_in("popup_cmd").len(),
        1,
        "not replayed again: {}",
        text(&r)
    );
    assert_eq!(page.invoked_in("main_cmd").len(), 2);
}
