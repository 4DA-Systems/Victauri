#![allow(dead_code)]

use std::sync::Arc;

use victauri_core::WindowState;
use victauri_plugin::VictauriState;
use victauri_plugin::bridge::WebviewBridge;

// ── Liveness-probe answering ───────────────────────────────────────────────
// Every MCP eval is now preceded by a tiny liveness probe (a `victauri_eval_callback`
// carrying `result:'"probe_ok"'`) so a dead/hung bridge fails fast (~2s) instead of
// blocking the full eval timeout (up to 30s). A callback-style mock bridge must
// answer that probe to behave like a healthy webview — otherwise the probe times
// out and the real eval never runs (and the suite pays 2s per eval-path test).

/// If `script` is the pre-eval liveness probe, resolve its pending callback so the
/// real eval proceeds. Returns `true` when it was a probe — the caller should then
/// `return Ok(())` without treating it as a real eval.
///
/// The resolution runs on a fresh OS thread (mirroring the real callback arriving
/// asynchronously) so it can `blocking_lock` the async pending-evals map without a
/// runtime-reentrancy panic.
pub fn answer_liveness_probe(
    script: &str,
    pending_evals: &victauri_plugin::PendingCallbacks,
) -> bool {
    if !script.contains("probe_ok") {
        return false;
    }
    if let Some(id) = extract_probe_id(script) {
        let pending = pending_evals.clone();
        std::thread::spawn(move || {
            let mut map = pending.blocking_lock();
            if let Some(tx) = map.remove(&id) {
                let _ = tx.send("\"probe_ok\"".to_string());
            }
        });
    }
    true
}

/// Extract the 36-char eval id from a probe script of the form `…id:"<uuid>"…`.
/// The probe uses no space after `id:` (distinguishing it from the eval wrapper's
/// `id: "<uuid>"`), and contains no user code, so this match is unambiguous.
fn extract_probe_id(script: &str) -> Option<String> {
    let start = script.find("id:\"")? + 4;
    script.get(start..start + 36).map(str::to_string)
}

// ── Shared Window State Builder ────────────────────────────────────────────

pub fn make_windows(labels: &[&str]) -> Vec<WindowState> {
    labels
        .iter()
        .map(|label| {
            WindowState::new(label.to_string())
                .with_title(format!("{label} title"))
                .with_url(format!("http://localhost/{label}"))
                .with_visible(true)
                .with_focused(labels.first() == Some(label))
                .with_maximized(false)
                .with_minimized(false)
                .with_fullscreen(false)
                .with_position(0, 0)
                .with_size(800, 600)
        })
        .collect()
}

// ── Test State Constructor ─────────────────────────────────────────────────

pub fn test_state() -> Arc<VictauriState> {
    Arc::new(VictauriState::for_tests())
}

// ── SimpleMockBridge ───────────────────────────────────────────────────────
// eval_webview returns Ok(()) — used by integration tests.

pub struct SimpleMockBridge {
    windows: Vec<WindowState>,
}

impl SimpleMockBridge {
    pub fn new(labels: &[&str]) -> Self {
        Self {
            windows: make_windows(labels),
        }
    }
}

impl WebviewBridge for SimpleMockBridge {
    fn eval_webview(&self, _label: Option<&str>, _script: &str) -> Result<(), String> {
        Ok(())
    }

    fn get_window_states(&self, label: Option<&str>) -> Vec<WindowState> {
        match label {
            Some(l) => self
                .windows
                .iter()
                .filter(|w| w.label == l)
                .cloned()
                .collect(),
            None => self.windows.clone(),
        }
    }

    fn list_window_labels(&self) -> Vec<String> {
        self.windows.iter().map(|w| w.label.clone()).collect()
    }

    fn get_native_handle(&self, _label: Option<&str>) -> Result<isize, String> {
        Err("native handle not available in mock".to_string())
    }

    fn manage_window(&self, _label: Option<&str>, action: &str) -> Result<String, String> {
        Ok(format!("{action} executed"))
    }

    fn resize_window(&self, _label: Option<&str>, _width: u32, _height: u32) -> Result<(), String> {
        Ok(())
    }

    fn move_window(&self, _label: Option<&str>, _x: i32, _y: i32) -> Result<(), String> {
        Ok(())
    }

    fn set_window_title(&self, _label: Option<&str>, _title: &str) -> Result<(), String> {
        Ok(())
    }
}

// ── RejectingMockBridge ────────────────────────────────────────────────────
// eval_webview returns Err(...) — used by adversarial tests.

pub struct RejectingMockBridge {
    labels: Vec<String>,
}

impl RejectingMockBridge {
    pub fn new(labels: &[&str]) -> Self {
        Self {
            labels: labels
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
        }
    }
}

impl WebviewBridge for RejectingMockBridge {
    fn eval_webview(&self, _label: Option<&str>, _script: &str) -> Result<(), String> {
        Err("eval not supported in MockBridge".to_string())
    }

    fn get_window_states(&self, label: Option<&str>) -> Vec<WindowState> {
        self.labels
            .iter()
            .filter(|l| label.is_none() || label == Some(l.as_str()))
            .map(|l| {
                WindowState::new(l.clone())
                    .with_title(format!("{l} title"))
                    .with_url(format!("http://localhost/{l}"))
                    .with_visible(true)
                    .with_focused(l == "main")
                    .with_maximized(false)
                    .with_minimized(false)
                    .with_fullscreen(false)
                    .with_position(0, 0)
                    .with_size(800, 600)
            })
            .collect()
    }

    fn list_window_labels(&self) -> Vec<String> {
        self.labels.clone()
    }

    fn get_native_handle(&self, _label: Option<&str>) -> Result<isize, String> {
        Err("native handle not available in tests".to_string())
    }

    fn manage_window(&self, label: Option<&str>, action: &str) -> Result<String, String> {
        let target = label.unwrap_or("main");
        if !self.labels.contains(&target.to_string()) {
            return Err(format!("window not found: {target}"));
        }
        match action {
            "minimize" | "maximize" | "close" | "focus" | "show" | "hide" | "fullscreen"
            | "unfullscreen" | "unminimize" | "unmaximize" | "always_on_top"
            | "not_always_on_top" => Ok(format!("{action} executed")),
            _ => Err(format!("unknown action: {action}")),
        }
    }

    fn resize_window(&self, label: Option<&str>, _width: u32, _height: u32) -> Result<(), String> {
        let target = label.unwrap_or("main");
        if !self.labels.contains(&target.to_string()) {
            return Err(format!("window not found: {target}"));
        }
        Ok(())
    }

    fn move_window(&self, label: Option<&str>, _x: i32, _y: i32) -> Result<(), String> {
        let target = label.unwrap_or("main");
        if !self.labels.contains(&target.to_string()) {
            return Err(format!("window not found: {target}"));
        }
        Ok(())
    }

    fn set_window_title(&self, label: Option<&str>, _title: &str) -> Result<(), String> {
        let target = label.unwrap_or("main");
        if !self.labels.contains(&target.to_string()) {
            return Err(format!("window not found: {target}"));
        }
        Ok(())
    }
}
