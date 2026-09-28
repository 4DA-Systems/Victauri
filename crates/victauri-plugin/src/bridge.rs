use tauri::{Manager, Runtime};
use victauri_core::WindowState;

/// Runtime-erased interface for webview and backend access, allowing the MCP
/// server to interact with Tauri windows and the application backend without
/// generic parameters.
///
/// Victauri implements this for `tauri::AppHandle`; it is public so tests (and embedders) can
/// supply a mock. **Stability contract:** any method added to this trait in a future release
/// will have a default implementation, so implementing it does not pin you to an exact
/// version. [`WindowState`] is `#[non_exhaustive]` — build one with
/// `WindowState::new(label).with_*(..)`.
pub trait WebviewBridge: Send + Sync {
    /// Execute JavaScript in the target webview (defaults to "main" or first visible window).
    ///
    /// # Errors
    ///
    /// Returns an error string if no matching window is found or the eval fails.
    fn eval_webview(&self, label: Option<&str>, script: &str) -> Result<(), String>;
    /// Retrieve the state of one or all windows (position, size, visibility, focus, URL).
    fn get_window_states(&self, label: Option<&str>) -> Vec<WindowState>;
    /// Like [`get_window_states`](Self::get_window_states), but distinguishes "could not ask"
    /// (the UI thread did not answer in time) from "no such window". A caller that reports a
    /// window as missing, or a list as empty, must use this.
    ///
    /// # Errors
    ///
    /// Returns an error string if the window states could not be obtained.
    fn try_get_window_states(&self, label: Option<&str>) -> Result<Vec<WindowState>, String> {
        Ok(self.get_window_states(label))
    }
    /// Return the labels of all open webview windows.
    fn list_window_labels(&self) -> Vec<String>;
    /// Like [`list_window_labels`](Self::list_window_labels), but distinguishes "could not
    /// ask" (e.g. the UI thread did not answer in time) from "there are no windows". A caller
    /// deciding that a window is GONE must use this — an empty list from a wedged UI is not
    /// evidence of anything.
    ///
    /// # Errors
    ///
    /// Returns an error string if the window list could not be obtained.
    fn try_list_window_labels(&self) -> Result<Vec<String>, String> {
        Ok(self.list_window_labels())
    }
    /// Like [`eval_webview`](Self::eval_webview), but returns the label of the window the
    /// script was actually delivered to (for `None`, the resolved default window). The default
    /// implementation returns the requested label, or an EMPTY string when it cannot know which
    /// window a `None` label resolved to — callers must treat empty as "unknown".
    ///
    /// # Errors
    ///
    /// Returns an error string if no matching window is found or the eval fails.
    fn eval_webview_resolved(&self, label: Option<&str>, script: &str) -> Result<String, String> {
        self.eval_webview(label, script)?;
        Ok(label.map(str::to_string).unwrap_or_default())
    }
    /// Return the platform-native window handle for screenshot capture.
    /// Windows: `HWND`, macOS: `CGWindowID` (window number), Linux: `X11` window ID.
    ///
    /// # Errors
    ///
    /// Returns an error string if no matching window is found or the handle type is unsupported.
    fn get_native_handle(&self, label: Option<&str>) -> Result<isize, String>;
    /// Perform a window management action (minimize, maximize, close, show, hide, etc.).
    ///
    /// # Errors
    ///
    /// Returns an error string if no matching window is found or the action fails.
    fn manage_window(&self, label: Option<&str>, action: &str) -> Result<String, String>;
    /// Set the logical size of a window in device-independent pixels.
    ///
    /// # Errors
    ///
    /// Returns an error string if no matching window is found or the resize fails.
    fn resize_window(&self, label: Option<&str>, width: u32, height: u32) -> Result<(), String>;
    /// Set the logical position of a window in device-independent pixels.
    ///
    /// # Errors
    ///
    /// Returns an error string if no matching window is found or the move fails.
    fn move_window(&self, label: Option<&str>, x: i32, y: i32) -> Result<(), String>;
    /// Set the title bar text of a window.
    ///
    /// # Errors
    ///
    /// Returns an error string if no matching window is found or the title change fails.
    fn set_window_title(&self, label: Option<&str>, title: &str) -> Result<(), String>;

    // ── Native (OS-level, trusted) input ───────────────────────────────────
    //
    // These deliver real OS input events (`isTrusted: true`), unlike the JS
    // bridge's synthetic events. They are needed for app handlers that gate on
    // `event.isTrusted` and for user-activation-gated browser APIs. Default
    // implementations return an error so platforms without support degrade
    // gracefully (callers fall back to synthetic input).

    /// Type Unicode text as trusted OS keyboard input into the focused element
    /// of the target window. The element must already hold focus.
    ///
    /// # Errors
    /// Returns an error if not supported on this platform or the window is missing.
    fn native_type_text(&self, _label: Option<&str>, _text: &str) -> Result<(), String> {
        Err(
            "native (trusted) keyboard input is not implemented on this platform; \
             use synthetic input via the `input` tool without `trusted`"
                .to_string(),
        )
    }

    /// Press a single named key (e.g. `Enter`, `Tab`, `Escape`, `ArrowDown`) as
    /// trusted OS keyboard input to the focused element of the target window.
    ///
    /// # Errors
    /// Returns an error if not supported on this platform or the key is unknown.
    fn native_key(&self, _label: Option<&str>, _key: &str) -> Result<(), String> {
        Err(
            "native (trusted) key input is not implemented on this platform; \
             use synthetic input via the `input` tool without `trusted`"
                .to_string(),
        )
    }

    /// Click at logical (CSS-pixel) coordinates within the target window's
    /// content area, as a trusted OS mouse event.
    ///
    /// # Errors
    /// Returns an error if not supported on this platform or the window is missing.
    fn native_click(&self, _label: Option<&str>, _x: f64, _y: f64) -> Result<(), String> {
        Err(
            "native (trusted) mouse input is not implemented on this platform; \
             use synthetic input via the `interact` tool"
                .to_string(),
        )
    }

    // ── Backend Access ─────────────────────────────────────────────────────

    /// Return the app's per-user data directory (e.g. `~/.local/share/<app>/`).
    ///
    /// # Errors
    ///
    /// Returns an error if the path cannot be resolved.
    fn app_data_dir(&self) -> Result<std::path::PathBuf, String> {
        Err("backend access not available".to_string())
    }

    /// Return the app's per-user config directory (e.g. `~/.config/<app>/`).
    ///
    /// # Errors
    ///
    /// Returns an error if the path cannot be resolved.
    fn app_config_dir(&self) -> Result<std::path::PathBuf, String> {
        Err("backend access not available".to_string())
    }

    /// Return the app's log directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the path cannot be resolved.
    fn app_log_dir(&self) -> Result<std::path::PathBuf, String> {
        Err("backend access not available".to_string())
    }

    /// Return the app's local data directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the path cannot be resolved.
    fn app_local_data_dir(&self) -> Result<std::path::PathBuf, String> {
        Err("backend access not available".to_string())
    }

    /// Return the Tauri app configuration as JSON.
    #[must_use]
    fn tauri_config(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
}

fn find_window<'a, R: Runtime>(
    windows: &'a std::collections::HashMap<String, tauri::WebviewWindow<R>>,
    label: Option<&str>,
) -> Result<&'a tauri::WebviewWindow<R>, String> {
    match label {
        Some(l) => windows
            .get(l)
            .ok_or_else(|| format!("window not found: {l}")),
        None => windows
            .get("main")
            // Deterministic fallbacks: `webview_windows()` is a HashMap whose iteration order
            // differs between calls, so "first visible" must be chosen by a stable key or two
            // consecutive calls can pick different windows.
            .or_else(|| {
                let mut visible: Vec<_> = windows
                    .iter()
                    .filter(|(_, w)| w.is_visible().unwrap_or(false))
                    .collect();
                visible.sort_by(|a, b| a.0.cmp(b.0));
                visible.first().map(|(_, w)| *w)
            })
            .or_else(|| windows.iter().min_by(|a, b| a.0.cmp(b.0)).map(|(_, w)| w))
            .ok_or_else(|| "no window available".to_string()),
    }
}

/// Serializes Victauri's main-thread round trips.
///
/// Concurrent `run_on_main_thread` round trips corrupt the process heap on Linux/WebKitGTK —
/// glibc aborts the app with `malloc(): unaligned tcache chunk detected` or `corrupted
/// double-linked list`. Measured on Ubuntu 24.04 + `WebKitGTK` 2.52, three concurrent
/// introspection loops for 8s: **5/8 runs died** unserialized, **8/8** with the tokio
/// `block_in_place` hand-off removed, and **0/28** once the whole round trip is serialized here.
///
/// The bisect that pinned it down, so a future reader does not re-do it:
/// * Not the reload. Reload-hammering with no introspection never died (0/4); introspection
///   with no reload at all died every time (4/4). The crash is unrelated to page reloads.
/// * Not the dispatch alone. Locking only around `run_on_main_thread` still died 4/8 — what
///   matters is how many round trips are in flight at once, not how the message is posted.
/// * Not load. One unthrottled loop issuing the same total number of calls never died (0/4);
///   three concurrent loops died. Concurrency is the variable, not volume.
/// * Not our HTTP/tokio layer. A tool that touches no webview (`get_memory_stats`) at the same
///   concurrency never died (0/5).
/// * Not new. The pre-rmcp-3.1.2 tree died at the same rate (6/8), so this long predates 0.8.8.
///
/// Serializing costs effectively nothing: the closures already execute one at a time on the
/// single main thread, so this only stops several round trips being in flight *around* it.
///
/// The lock is only taken OFF the main thread: a caller already on the main thread (including
/// an `on_main` closure that calls back into the bridge) runs inline without it — see `on_main`.
static MAIN_DISPATCH_LOCK: DispatchGate = DispatchGate::new();

/// The serialization state behind [`MAIN_DISPATCH_LOCK`] (a separate type so tests can use
/// their own gate instead of the process-wide one).
///
/// The lock alone is not enough: a caller whose closure STARTED but outlived its timeout + grace
/// returns "outcome unknown" and releases the lock while that closure is still executing on the
/// main thread. `in_flight` counts dispatched closures that have reached the main thread and not
/// yet finished, and the next lock holder also waits (within its own deadline) for it to reach
/// zero — so there is still never more than one round trip in flight (R4-LOCK1).
struct DispatchGate {
    lock: std::sync::Mutex<()>,
    in_flight: std::sync::atomic::AtomicUsize,
}

impl DispatchGate {
    const fn new() -> Self {
        Self {
            lock: std::sync::Mutex::new(()),
            in_flight: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

/// Decrements [`DispatchGate::in_flight`] when a dispatched closure finishes — including by
/// unwinding, so a panicking closure can never wedge every later round trip.
struct InFlightGuard(&'static DispatchGate);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0
            .in_flight
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// The Tauri main (UI) thread, recorded by the plugin's `setup` (which Tauri runs there).
static MAIN_THREAD: std::sync::OnceLock<std::thread::ThreadId> = std::sync::OnceLock::new();

/// Record the calling thread as the Tauri main thread. Called once from plugin `setup`.
pub(crate) fn record_main_thread() {
    let _ = MAIN_THREAD.set(std::thread::current().id());
}

/// Whether the current thread is the recorded Tauri main thread (false if not yet recorded).
fn is_main_thread() -> bool {
    MAIN_THREAD
        .get()
        .is_some_and(|id| *id == std::thread::current().id())
}

/// Run `f` on the Tauri **main (UI) thread** and return its result.
///
/// Every webview/window access MUST happen on the main thread. Tauri's window/webview
/// handles wrap a non-`Send` `Rc<WebView>` (and a `RefCell`-backed window store) that are
/// guarded only by an `unsafe impl Send` with a *main-thread-only* contract. The Victauri
/// MCP server runs on a background (axum/tokio) thread, so touching those handles directly —
/// e.g. `self.webview_windows()` cloning the `Rc` — races the main thread's own refcounting
/// (notably `tauri::ipc::protocol::get` while the app handles its real IPC). Two threads
/// mutating a non-atomic `Rc` count corrupts it → use-after-free, which surfaces as
/// `STATUS_*_BUFFER_OVERRUN` once Rust's debug `assert_unchecked` on `Rc::inc_strong`
/// (1.78+) starts checking it. See Tauri issue #10001 for the identical crash class.
///
/// `run_on_main_thread` marshals the closure onto the UI thread (and runs it inline if we are
/// already on it), so all `Rc` access stays single-threaded. The closure's value comes back
/// over a oneshot `std::sync::mpsc` channel; the bounded `recv` is a safety net against a
/// wedged event loop and never blocks the main thread (the closure runs *there*, the wait
/// happens on the calling background thread).
///
/// The closure runs ON the UI/event-loop thread, so a panic in it would unwind into tao/winit
/// and **abort the whole process** — strictly worse than the use-after-free this dispatcher
/// exists to prevent — and would skip the result send, hanging the caller the full timeout. So
/// the closure is run under `catch_unwind` and a panic is converted into an error that is always
/// sent back. (Today every closure is panic-free by construction, but the helper must not let a
/// future one take the app down.)
///
/// Round trips are additionally serialized through [`MAIN_DISPATCH_LOCK`] — see there for the
/// heap corruption that requires it and the bisect that established it.
fn on_main<R, T, F>(app: &tauri::AppHandle<R>, what: &str, f: F) -> Result<T, String>
where
    R: Runtime,
    T: Send + 'static,
    F: FnOnce(&tauri::AppHandle<R>) -> T + Send + 'static,
{
    let timeout = std::time::Duration::from_secs(10);

    // Already ON the main thread (a sync Tauri command, a menu handler, or an `on_main` closure
    // that calls back into the bridge): run inline, WITHOUT the dispatch lock. Taking the lock
    // here could deadlock — a background holder waits for its closure, which is queued behind
    // us on this very thread — freezing the UI for the full timeout. Skipping it is safe: no
    // other closure can execute on this thread concurrently, and the heap corruption the lock
    // prevents needs several cross-thread round trips in flight, which an inline call is not.
    if is_main_thread() {
        return std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(app)))
            .map_err(|_| format!("{what} panicked on the main thread"));
    }

    let round_trip = move || -> Result<T, String> {
        let app_for_closure = app.clone();
        serialized_round_trip(
            &MAIN_DISPATCH_LOCK,
            what,
            timeout,
            |job| {
                app.run_on_main_thread(job)
                    .map_err(|e| format!("failed to dispatch {what} to the main thread: {e}"))
            },
            move || f(&app_for_closure),
        )
    };

    // Blocking here must not park a tokio runtime worker — under a wedged UI that could
    // otherwise starve the embedded axum/MCP server. On a multi-threaded runtime, `block_in_place`
    // tells the scheduler to run other tasks elsewhere while this thread blocks. (It panics on a
    // current-thread runtime, so guard on the flavor; with no runtime at all, just block.)
    match tokio::runtime::Handle::try_current().map(|h| h.runtime_flavor()) {
        Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(round_trip),
        _ => round_trip(),
    }
}

/// One serialized round trip: take `gate`'s lock (see [`MAIN_DISPATCH_LOCK`]), then hand `f` to `post`
/// and wait for it. `timeout` bounds the WHOLE call, including the wait for the lock, so
/// serializing cannot stack N callers into N * timeout when the UI wedges: each caller still
/// gives up after `timeout` total, exactly as it did before the lock existed.
fn serialized_round_trip<T, F>(
    gate: &'static DispatchGate,
    what: &str,
    timeout: std::time::Duration,
    post: impl FnOnce(Box<dyn FnOnce() + Send>) -> Result<(), String>,
    f: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let deadline = std::time::Instant::now() + timeout;
    let lock_timeout = || {
        format!(
            "{what} did not complete on the main thread: timed out waiting for the main-thread \
             dispatch lock"
        )
    };
    let _serialize = loop {
        match gate.lock.try_lock() {
            Ok(guard) => break guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return Err(lock_timeout());
                }
                std::thread::sleep(remaining.min(std::time::Duration::from_millis(2)));
            }
        }
    };
    // An earlier caller may have given up on a closure that is STILL RUNNING on the main thread
    // (it started, then outlived that caller's timeout + grace). Wait for it — bounded by this
    // caller's own deadline, so callers never stack into N * timeout — before putting a second
    // round trip in flight beside it.
    while gate.in_flight.load(std::sync::atomic::Ordering::Acquire) != 0 {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "{what} did not complete on the main thread: timed out waiting for an earlier                  main-thread call that is still running (it will not run)"
            ));
        }
        std::thread::sleep(remaining.min(std::time::Duration::from_millis(2)));
    }
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return Err(lock_timeout());
    }
    // Count the closure from BEFORE it can start (the count is raised ahead of the
    // queued->running transition inside `dispatch_and_wait`'s job), so once a caller has seen
    // its job start, the next lock holder is guaranteed to see it in flight.
    let counted_post = move |job: Box<dyn FnOnce() + Send>| {
        post(Box::new(move || {
            gate.in_flight
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let _done = InFlightGuard(gate);
            job();
        }))
    };
    // Returns with the lock still held until the closure has either run or been abandoned,
    // so no second round trip is ever in flight beside a closure that may still start.
    dispatch_and_wait(what, remaining, timeout, counted_post, f)
}

/// A dispatched closure that has not started yet.
const JOB_QUEUED: u8 = 0;
/// A dispatched closure that has started (its effects will happen).
const JOB_RUNNING: u8 = 1;
/// A dispatched closure its caller gave up on before it started: it must never run.
const JOB_ABANDONED: u8 = 2;

/// Hand `f` to `post` (which queues it on the main thread) and wait up to `remaining` for it.
///
/// The closure and a caller that times out race through ONE compare-and-swap: the closure only
/// runs if it moves Queued -> Running first, and the caller only reports a timeout if it moves
/// Queued -> Abandoned first. So a timeout error always means the work never happened and never
/// will — a state-mutating op (a resize, move, close, title change or eval) cannot apply after
/// the caller already saw the error and moved on. A closure that started just before the
/// deadline is waited for (up to `grace`) and its real outcome returned, instead of "timed out"
/// for work that ran.
fn dispatch_and_wait<T, F>(
    what: &str,
    remaining: std::time::Duration,
    grace: std::time::Duration,
    post: impl FnOnce(Box<dyn FnOnce() + Send>) -> Result<(), String>,
    f: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::mpsc::RecvTimeoutError;

    let (tx, rx) = std::sync::mpsc::channel();
    let state = std::sync::Arc::new(AtomicU8::new(JOB_QUEUED));
    let job_state = std::sync::Arc::clone(&state);
    post(Box::new(move || {
        if job_state
            .compare_exchange(JOB_QUEUED, JOB_RUNNING, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // Send only fails if the caller already gave up and dropped the receiver — ignore.
        let _ = tx.send(result);
    }))?;

    let outcome = match rx.recv_timeout(remaining) {
        Ok(outcome) => outcome,
        Err(RecvTimeoutError::Disconnected) => {
            return Err(format!(
                "{what} did not complete on the main thread: the event loop dropped it"
            ));
        }
        Err(RecvTimeoutError::Timeout) => {
            if state
                .compare_exchange(
                    JOB_QUEUED,
                    JOB_ABANDONED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return Err(format!(
                    "{what} did not complete on the main thread: timed out before it started \
                     (it will not run)"
                ));
            }
            // It started before we gave up: its effects will happen, so report its outcome.
            rx.recv_timeout(grace).map_err(|_| {
                format!(
                    "{what} started on the main thread but did not finish in time; its outcome \
                     is unknown"
                )
            })?
        }
    };
    outcome.map_err(|_panic| format!("{what} panicked on the main thread"))
}

impl<R: Runtime> WebviewBridge for tauri::AppHandle<R> {
    fn eval_webview(&self, label: Option<&str>, script: &str) -> Result<(), String> {
        let label = label.map(str::to_string);
        let script = script.to_string();
        on_main(self, "eval_webview", move |app| {
            let windows = app.webview_windows();
            let webview = find_window(&windows, label.as_deref())?;
            webview.eval(&script).map_err(|e| e.to_string())
        })?
    }

    fn get_window_states(&self, label: Option<&str>) -> Vec<WindowState> {
        // An empty Vec here means the main-thread dispatch failed/timed out (a wedged UI), not
        // "no windows" — log it so that case is diagnosable rather than silently indistinguishable.
        self.try_get_window_states(label).unwrap_or_else(|e| {
            tracing::warn!("get_window_states: {e}");
            Vec::new()
        })
    }

    fn try_get_window_states(&self, label: Option<&str>) -> Result<Vec<WindowState>, String> {
        let label = label.map(str::to_string);
        on_main(self, "get_window_states", move |app| {
            let windows = app.webview_windows();
            let mut states = Vec::new();

            for (win_label, window) in &windows {
                if let Some(filter) = label.as_deref()
                    && win_label != filter
                {
                    continue;
                }

                let pos = window.outer_position().unwrap_or_default();
                let size = window.inner_size().unwrap_or_default();

                states.push(
                    WindowState::new(win_label.clone())
                        .with_title(window.title().unwrap_or_default())
                        .with_url(window.url().map(|u| u.to_string()).unwrap_or_default())
                        .with_visible(window.is_visible().unwrap_or(false))
                        .with_focused(window.is_focused().unwrap_or(false))
                        .with_maximized(window.is_maximized().unwrap_or(false))
                        .with_minimized(window.is_minimized().unwrap_or(false))
                        .with_fullscreen(window.is_fullscreen().unwrap_or(false))
                        .with_position(pos.x, pos.y)
                        .with_size(size.width, size.height),
                );
            }

            states
        })
    }

    fn list_window_labels(&self) -> Vec<String> {
        self.try_list_window_labels().unwrap_or_else(|e| {
            tracing::warn!("list_window_labels: {e}");
            Vec::new()
        })
    }

    fn try_list_window_labels(&self) -> Result<Vec<String>, String> {
        on_main(self, "list_window_labels", |app| {
            app.webview_windows().keys().cloned().collect()
        })
    }

    fn eval_webview_resolved(&self, label: Option<&str>, script: &str) -> Result<String, String> {
        let label = label.map(str::to_string);
        let script = script.to_string();
        on_main(self, "eval_webview", move |app| {
            let windows = app.webview_windows();
            let webview = find_window(&windows, label.as_deref())?;
            webview.eval(&script).map_err(|e| e.to_string())?;
            Ok(webview.label().to_string())
        })?
    }

    fn get_native_handle(&self, label: Option<&str>) -> Result<isize, String> {
        let label = label.map(str::to_string);
        on_main(self, "get_native_handle", move |app| {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};

            let windows = app.webview_windows();
            let _webview = find_window(&windows, label.as_deref())?;
            let handle = _webview.window_handle().map_err(|e| e.to_string())?;
            match handle.as_raw() {
                #[cfg(windows)]
                RawWindowHandle::Win32(h) => Ok(h.hwnd.get()),
                #[cfg(target_os = "macos")]
                RawWindowHandle::AppKit(h) => {
                    // CGWindowListCreateImage needs CGWindowID (the window number),
                    // not the NSView pointer. Extract via Objective-C runtime.
                    macos_window_number(h.ns_view.as_ptr())
                }
                #[cfg(target_os = "linux")]
                RawWindowHandle::Xlib(h) => Ok(h.window as isize),
                #[cfg(target_os = "linux")]
                RawWindowHandle::Xcb(h) => Ok(h.window.get() as isize),
                _ => Err("unsupported window handle type on this platform".to_string()),
            }
        })?
    }

    #[cfg(windows)]
    fn native_type_text(&self, label: Option<&str>, text: &str) -> Result<(), String> {
        let hwnd = self.get_native_handle(label)?;
        win_focus(hwnd)?;
        win_send_text(text)
    }

    #[cfg(windows)]
    fn native_key(&self, label: Option<&str>, key: &str) -> Result<(), String> {
        let hwnd = self.get_native_handle(label)?;
        win_focus(hwnd)?;
        win_send_key(key)
    }

    #[cfg(windows)]
    fn native_click(&self, label: Option<&str>, x: f64, y: f64) -> Result<(), String> {
        let hwnd = self.get_native_handle(label)?;
        win_focus(hwnd)?;
        win_click(hwnd, x, y)
    }

    fn manage_window(&self, label: Option<&str>, action: &str) -> Result<String, String> {
        let label = label.map(str::to_string);
        let action = action.to_string();
        on_main(self, "manage_window", move |app| {
            let windows = app.webview_windows();
            let window = find_window(&windows, label.as_deref())?;

            match action.as_str() {
                "minimize" => window.minimize().map_err(|e| e.to_string())?,
                "unminimize" => window.unminimize().map_err(|e| e.to_string())?,
                "maximize" => window.maximize().map_err(|e| e.to_string())?,
                "unmaximize" => window.unmaximize().map_err(|e| e.to_string())?,
                "close" => window.close().map_err(|e| e.to_string())?,
                "focus" => window.set_focus().map_err(|e| e.to_string())?,
                "show" => window.show().map_err(|e| e.to_string())?,
                "hide" => window.hide().map_err(|e| e.to_string())?,
                "fullscreen" => window.set_fullscreen(true).map_err(|e| e.to_string())?,
                "unfullscreen" => window.set_fullscreen(false).map_err(|e| e.to_string())?,
                "always_on_top" => window.set_always_on_top(true).map_err(|e| e.to_string())?,
                "not_always_on_top" => {
                    window.set_always_on_top(false).map_err(|e| e.to_string())?;
                }
                _ => return Err(format!("unknown action: {action}")),
            }

            Ok(format!("{action} executed"))
        })?
    }

    fn resize_window(&self, label: Option<&str>, width: u32, height: u32) -> Result<(), String> {
        let label = label.map(str::to_string);
        on_main(self, "resize_window", move |app| {
            let windows = app.webview_windows();
            let window = find_window(&windows, label.as_deref())?;

            window
                .set_size(tauri::LogicalSize::new(width, height))
                .map_err(|e| e.to_string())
        })?
    }

    fn move_window(&self, label: Option<&str>, x: i32, y: i32) -> Result<(), String> {
        let label = label.map(str::to_string);
        on_main(self, "move_window", move |app| {
            let windows = app.webview_windows();
            let window = find_window(&windows, label.as_deref())?;

            window
                .set_position(tauri::LogicalPosition::new(x, y))
                .map_err(|e| e.to_string())
        })?
    }

    fn set_window_title(&self, label: Option<&str>, title: &str) -> Result<(), String> {
        let label = label.map(str::to_string);
        let title = title.to_string();
        on_main(self, "set_window_title", move |app| {
            let windows = app.webview_windows();
            let window = find_window(&windows, label.as_deref())?;

            window.set_title(&title).map_err(|e| e.to_string())
        })?
    }

    fn app_data_dir(&self) -> Result<std::path::PathBuf, String> {
        self.path().app_data_dir().map_err(|e| e.to_string())
    }

    fn app_config_dir(&self) -> Result<std::path::PathBuf, String> {
        self.path().app_config_dir().map_err(|e| e.to_string())
    }

    fn app_log_dir(&self) -> Result<std::path::PathBuf, String> {
        self.path().app_log_dir().map_err(|e| e.to_string())
    }

    fn app_local_data_dir(&self) -> Result<std::path::PathBuf, String> {
        self.path().app_local_data_dir().map_err(|e| e.to_string())
    }

    fn tauri_config(&self) -> serde_json::Value {
        let config = self.config();

        let windows: Vec<serde_json::Value> = config
            .app
            .windows
            .iter()
            .map(|w| {
                serde_json::json!({
                    "label": w.label,
                    "title": w.title,
                    "url": format!("{}", w.url),
                    "width": w.width,
                    "height": w.height,
                    "visible": w.visible,
                    "resizable": w.resizable,
                    "fullscreen": w.fullscreen,
                    "decorations": w.decorations,
                    "transparent": w.transparent,
                    "always_on_top": w.always_on_top,
                })
            })
            .collect();

        let plugins: Vec<String> = config.plugins.0.keys().cloned().collect();

        let security = serde_json::json!({
            "csp": config.app.security.csp.as_ref().map(|c| format!("{c}")),
            "freeze_prototype": config.app.security.freeze_prototype,
            "capabilities": config.app.security.capabilities.iter().map(|c| {
                match c {
                    tauri::utils::config::CapabilityEntry::Inlined(cap) => {
                        serde_json::json!({
                            "identifier": cap.identifier,
                            "description": cap.description,
                            "windows": cap.windows,
                            "webviews": cap.webviews,
                            "permissions": cap.permissions.iter().map(|p| format!("{p:?}")).collect::<Vec<_>>(),
                            "platforms": cap.platforms,
                        })
                    }
                    tauri::utils::config::CapabilityEntry::Reference(path) => {
                        serde_json::json!({ "reference": path })
                    }
                }
            }).collect::<Vec<_>>(),
        });

        serde_json::json!({
            "identifier": config.identifier,
            "product_name": config.product_name,
            "version": config.version,
            "windows": windows,
            "plugins": plugins,
            "security": security,
        })
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn macos_window_number(ns_view: *mut std::ffi::c_void) -> Result<isize, String> {
    unsafe extern "C" {
        fn objc_msgSend(obj: *mut std::ffi::c_void, sel: *mut std::ffi::c_void) -> isize;
        fn sel_registerName(name: *const std::ffi::c_char) -> *mut std::ffi::c_void;
    }

    if ns_view.is_null() {
        return Err("null NSView handle".to_string());
    }

    // SAFETY: `ns_view` is a valid NSView pointer obtained from Tauri's
    // `with_webview` callback; null was checked above. `objc_msgSend` and
    // `sel_registerName` are stable Objective-C runtime ABI.
    unsafe {
        let sel_window = sel_registerName(c"window".as_ptr());
        let ns_window = objc_msgSend(ns_view, sel_window);
        if ns_window == 0 {
            return Err("NSView has no parent NSWindow".to_string());
        }
        let sel_window_number = sel_registerName(c"windowNumber".as_ptr());
        let ns_window_ptr = ns_window as *mut std::ffi::c_void;
        let window_number = objc_msgSend(ns_window_ptr, sel_window_number);
        if window_number <= 0 {
            return Err(format!("invalid CGWindowID: {window_number}"));
        }
        Ok(window_number)
    }
}

// ── Windows native (trusted) input helpers ─────────────────────────────────
//
// These deliver real OS input via SendInput, producing events with
// `isTrusted: true` (unlike the JS bridge's synthetic events).

#[cfg(windows)]
fn win_hwnd(hwnd: isize) -> windows::Win32::Foundation::HWND {
    windows::Win32::Foundation::HWND(hwnd as *mut core::ffi::c_void)
}

/// `SendInput` goes to whatever window has the foreground, not to a window we name: trusted
/// input may only be sent once the target's top-level window IS the foreground window.
#[cfg(any(windows, test))]
fn foreground_verdict(target_root: isize, foreground_root: isize) -> Result<(), String> {
    if target_root != 0 && target_root == foreground_root {
        Ok(())
    } else {
        Err(
            "refusing trusted input: the app window could not be brought to the foreground \
             (Windows' foreground lock keeps focus with the app the user is working in, e.g. a \
             terminal), so the keystrokes or click would go to that app instead. Bring the app \
             window to the front and retry, or omit `trusted` to use synthetic input."
                .to_string(),
        )
    }
}

/// A trusted click lands on whatever window is topmost at the point: it must be the target.
#[cfg(any(windows, test))]
fn click_target_verdict(target_root: isize, point_root: isize) -> Result<(), String> {
    if target_root != 0 && target_root == point_root {
        Ok(())
    } else {
        Err(
            "refusing trusted click: the point is covered by another window (or lies outside \
             this one), so the click would land there. Make the element visible on screen and \
             retry, or omit `trusted` to use a synthetic click."
                .to_string(),
        )
    }
}

/// The top-level window `hwnd` belongs to (0 for none).
#[allow(unsafe_code)]
#[cfg(windows)]
fn win_root(hwnd: windows::Win32::Foundation::HWND) -> isize {
    use windows::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor};
    // SAFETY: GetAncestor accepts any HWND (including null) and returns null when there is none.
    unsafe { GetAncestor(hwnd, GA_ROOT) }.0 as isize
}

/// Bring the target window to the foreground so input is routed to it, and verify it got
/// there. `SetForegroundWindow` is refused while the user is active in another app (Windows'
/// foreground lock); ignoring that sent the agent's keystrokes — Enter included — into the
/// developer's terminal.
#[allow(unsafe_code)]
#[cfg(windows)]
fn win_focus(hwnd: isize) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};
    let target = win_hwnd(hwnd);
    let target_root = win_root(target);
    // SAFETY: hwnd comes from Tauri's window handle; SetForegroundWindow is safe
    // to call with any HWND (returns false if it fails).
    unsafe {
        let _ = SetForegroundWindow(target);
    }
    // Give the OS a brief moment to apply focus, then poll briefly for it to take.
    std::thread::sleep(std::time::Duration::from_millis(40));
    let mut verdict = Ok(());
    for _ in 0..8 {
        // SAFETY: GetForegroundWindow takes no arguments and may return null.
        verdict = foreground_verdict(target_root, win_root(unsafe { GetForegroundWindow() }));
        if verdict.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    verdict
}

#[cfg(windows)]
fn win_keyboard_input(
    vk: u16,
    scan: u16,
    key_up: bool,
    unicode: bool,
) -> windows::Win32::UI::Input::KeyboardAndMouse::INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
        KEYEVENTF_UNICODE, VIRTUAL_KEY,
    };
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if unicode {
        flags |= KEYEVENTF_UNICODE;
    }
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Type Unicode text via `SendInput` (`KEYEVENTF_UNICODE` per UTF-16 code unit).
#[allow(unsafe_code)]
#[cfg(windows)]
fn win_send_text(text: &str) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, SendInput};
    let mut inputs: Vec<INPUT> = Vec::new();
    for unit in text.encode_utf16() {
        inputs.push(win_keyboard_input(0, unit, false, true));
        inputs.push(win_keyboard_input(0, unit, true, true));
    }
    if inputs.is_empty() {
        return Ok(());
    }
    let cb = i32::try_from(std::mem::size_of::<INPUT>()).unwrap_or(0);
    // SAFETY: `inputs` is a valid slice of properly-initialized INPUT structs.
    let sent = unsafe { SendInput(&inputs, cb) } as usize;
    if sent == inputs.len() {
        Ok(())
    } else {
        Err(format!(
            "SendInput delivered {sent}/{} key events",
            inputs.len()
        ))
    }
}

/// Map a named key (Playwright-style) to a Win32 virtual-key code.
#[cfg(windows)]
fn win_vk_for_key(key: &str) -> Option<u16> {
    use windows::Win32::UI::Input::KeyboardAndMouse as k;
    let vk = match key {
        "Enter" | "Return" => k::VK_RETURN,
        "Tab" => k::VK_TAB,
        "Escape" | "Esc" => k::VK_ESCAPE,
        "Backspace" => k::VK_BACK,
        "Delete" | "Del" => k::VK_DELETE,
        "ArrowUp" | "Up" => k::VK_UP,
        "ArrowDown" | "Down" => k::VK_DOWN,
        "ArrowLeft" | "Left" => k::VK_LEFT,
        "ArrowRight" | "Right" => k::VK_RIGHT,
        "Home" => k::VK_HOME,
        "End" => k::VK_END,
        "PageUp" => k::VK_PRIOR,
        "PageDown" => k::VK_NEXT,
        "Space" | " " => k::VK_SPACE,
        "F1" => k::VK_F1,
        "F2" => k::VK_F2,
        "F3" => k::VK_F3,
        "F4" => k::VK_F4,
        "F5" => k::VK_F5,
        "F6" => k::VK_F6,
        "F7" => k::VK_F7,
        "F8" => k::VK_F8,
        "F9" => k::VK_F9,
        "F10" => k::VK_F10,
        "F11" => k::VK_F11,
        "F12" => k::VK_F12,
        _ => return None,
    };
    Some(vk.0)
}

/// Press and release a named key, or a single printable character, via `SendInput`.
#[allow(unsafe_code)]
#[cfg(windows)]
fn win_send_key(key: &str) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, SendInput};
    let inputs: Vec<INPUT> = if let Some(vk) = win_vk_for_key(key) {
        vec![
            win_keyboard_input(vk, 0, false, false),
            win_keyboard_input(vk, 0, true, false),
        ]
    } else {
        // Single printable character → send as Unicode.
        let mut chars = key.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return Err(format!(
                "unknown key '{key}' (use a named key or a single character)"
            ));
        };
        let mut buf = [0u16; 2];
        let mut v = Vec::new();
        for unit in c.encode_utf16(&mut buf) {
            v.push(win_keyboard_input(0, *unit, false, true));
            v.push(win_keyboard_input(0, *unit, true, true));
        }
        v
    };
    let cb = i32::try_from(std::mem::size_of::<INPUT>()).unwrap_or(0);
    // SAFETY: valid slice of initialized INPUT structs.
    let sent = unsafe { SendInput(&inputs, cb) } as usize;
    if sent == inputs.len() {
        Ok(())
    } else {
        Err(format!(
            "SendInput delivered {sent}/{} key events",
            inputs.len()
        ))
    }
}

/// Click at logical (CSS-pixel) coordinates within the window's content area
/// via an absolute-positioned `SendInput` mouse sequence (move + down + up).
#[allow(unsafe_code)]
#[cfg(windows)]
fn win_click(hwnd: isize, x: f64, y: f64) -> Result<(), String> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_MOUSE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, SendInput,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN, WindowFromPoint,
    };
    let h = win_hwnd(hwnd);
    // SAFETY: GetDpiForWindow/GetSystemMetrics/ClientToScreen are safe to call
    // with a valid HWND; ClientToScreen writes into our stack POINT.
    let (nx, ny) = unsafe {
        let dpi = GetDpiForWindow(h);
        let scale = if dpi == 0 { 1.0 } else { f64::from(dpi) / 96.0 };
        let mut pt = POINT {
            x: (x * scale) as i32,
            y: (y * scale) as i32,
        };
        let _ = ClientToScreen(h, &mut pt);
        // The click goes to the topmost window at the point, whichever app that is.
        click_target_verdict(win_root(h), win_root(WindowFromPoint(pt)))?;
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if vw <= 1 || vh <= 1 {
            return Err("virtual screen metrics unavailable".to_string());
        }
        let nx = ((f64::from(pt.x - vx)) * 65535.0 / f64::from(vw - 1)) as i32;
        let ny = ((f64::from(pt.y - vy)) * 65535.0 / f64::from(vh - 1)) as i32;
        (nx, ny)
    };
    let make = |flags: MOUSE_EVENT_FLAGS| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: nx,
                dy: ny,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let base = MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
    let inputs = [
        make(base | MOUSEEVENTF_MOVE),
        make(base | MOUSEEVENTF_LEFTDOWN),
        make(base | MOUSEEVENTF_LEFTUP),
    ];
    let cb = i32::try_from(std::mem::size_of::<INPUT>()).unwrap_or(0);
    // SAFETY: valid slice of initialized INPUT structs.
    let sent = unsafe { SendInput(&inputs, cb) } as usize;
    if sent == inputs.len() {
        Ok(())
    } else {
        Err(format!(
            "SendInput delivered {sent}/{} mouse events",
            inputs.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{dispatch_and_wait, is_main_thread, record_main_thread};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// A stand-in main thread that starts each posted job after `delay`.
    fn post_after(delay: Duration) -> impl FnOnce(Box<dyn FnOnce() + Send>) -> Result<(), String> {
        move |job| {
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                job();
            });
            Ok(())
        }
    }

    #[test]
    fn a_job_still_queued_at_the_deadline_never_runs() {
        let ran = Arc::new(AtomicUsize::new(0));
        let r = Arc::clone(&ran);
        let out = dispatch_and_wait(
            "resize_window",
            Duration::from_millis(50),
            Duration::from_secs(5),
            post_after(Duration::from_millis(300)),
            move || r.fetch_add(1, Ordering::SeqCst),
        );
        assert!(out.unwrap_err().contains("will not run"));
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(ran.load(Ordering::SeqCst), 0, "an abandoned job ran anyway");
    }

    #[test]
    fn a_job_running_at_the_deadline_reports_its_real_outcome() {
        // It started before the caller gave up, so its effects happen: reporting "timed out"
        // would tell the caller a window change / eval did not happen when it did.
        let out = dispatch_and_wait(
            "eval_webview",
            Duration::from_millis(50),
            Duration::from_secs(5),
            post_after(Duration::ZERO),
            || {
                std::thread::sleep(Duration::from_millis(300));
                7
            },
        );
        assert_eq!(out, Ok(7));
    }

    #[test]
    fn trusted_input_is_refused_unless_the_target_is_in_front() {
        use super::{click_target_verdict, foreground_verdict};
        assert!(foreground_verdict(0x42, 0x42).is_ok());
        let other = foreground_verdict(0x42, 0x99).unwrap_err();
        assert!(other.contains("foreground"), "{other}");
        assert!(foreground_verdict(0x42, 0).is_err(), "no foreground window");
        assert!(foreground_verdict(0, 0).is_err(), "unknown target window");
        assert!(click_target_verdict(0x42, 0x42).is_ok());
        assert!(
            click_target_verdict(0x42, 0x99)
                .unwrap_err()
                .contains("covered")
        );
        assert!(click_target_verdict(0, 0).is_err());
    }

    fn leaked_gate() -> &'static super::DispatchGate {
        Box::leak(Box::new(super::DispatchGate::new()))
    }

    /// R4-LOCK1: a round trip whose closure STARTED but outlived timeout + grace returned
    /// "outcome unknown" and released the dispatch lock while the closure was still running on
    /// the main thread — so the next caller put a second round trip in flight beside it, the
    /// exact condition `MAIN_DISPATCH_LOCK` exists to prevent (`WebKitGTK` heap corruption).
    #[test]
    fn a_job_outliving_its_caller_blocks_the_next_round_trip_until_it_finishes() {
        use super::serialized_round_trip;
        use std::time::Instant;
        let gate = leaked_gate();
        let first_end = Arc::new(std::sync::Mutex::new(None::<Instant>));
        let fe = Arc::clone(&first_end);
        let first = serialized_round_trip(
            gate,
            "first",
            Duration::from_millis(50),
            post_after(Duration::ZERO),
            move || {
                std::thread::sleep(Duration::from_millis(400));
                *fe.lock().unwrap() = Some(Instant::now());
            },
        );
        assert!(
            first.unwrap_err().contains("outcome is unknown"),
            "precondition: the first caller gave up while its job was running"
        );
        let second_start = serialized_round_trip(
            gate,
            "second",
            Duration::from_secs(5),
            post_after(Duration::ZERO),
            Instant::now,
        )
        .expect("the second round trip runs once the first job is done");
        let first_end = first_end
            .lock()
            .unwrap()
            .expect("the first job finished before the second started");
        assert!(
            second_start >= first_end,
            "second round trip started {:?} before the abandoned first job finished",
            first_end - second_start
        );
    }

    /// The wait for an earlier caller's still-running job is bounded by the caller's OWN
    /// deadline (no N * timeout stacking), and a caller that gives up never runs its job.
    #[test]
    fn waiting_for_an_abandoned_job_is_bounded_by_the_callers_deadline() {
        use super::serialized_round_trip;
        let gate = leaked_gate();
        let first = serialized_round_trip(
            gate,
            "first",
            Duration::from_millis(30),
            post_after(Duration::ZERO),
            || std::thread::sleep(Duration::from_millis(1500)),
        );
        assert!(first.unwrap_err().contains("outcome is unknown"));
        let ran = Arc::new(AtomicUsize::new(0));
        let r = Arc::clone(&ran);
        let started = std::time::Instant::now();
        let second = serialized_round_trip(
            gate,
            "second",
            Duration::from_millis(200),
            post_after(Duration::ZERO),
            move || r.fetch_add(1, Ordering::SeqCst),
        );
        let waited = started.elapsed();
        let err = second.unwrap_err();
        assert!(err.contains("still running"), "{err}");
        assert!(waited < Duration::from_millis(1000), "waited {waited:?}");
        std::thread::sleep(Duration::from_millis(1600));
        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "a caller that gave up ran its job"
        );
    }

    #[test]
    fn a_panicking_job_is_an_error_not_a_hang() {
        let out: Result<(), String> = dispatch_and_wait(
            "get_native_handle",
            Duration::from_secs(5),
            Duration::from_secs(5),
            post_after(Duration::ZERO),
            || panic!("boom"),
        );
        assert!(out.unwrap_err().contains("panicked"));
    }

    #[test]
    fn main_thread_is_recorded_once_and_only_that_thread_matches() {
        // Record from a dedicated thread (standing in for Tauri's setup thread): only that
        // thread is "main"; every other thread — including this test's — is not.
        std::thread::spawn(|| {
            record_main_thread();
            assert!(is_main_thread(), "the recording thread is the main thread");
        })
        .join()
        .unwrap();
        assert!(
            !is_main_thread(),
            "a different thread must not be treated as main"
        );
        std::thread::spawn(|| assert!(!is_main_thread()))
            .join()
            .unwrap();
        // A second record from another thread must not move it (set-once).
        std::thread::spawn(record_main_thread).join().unwrap();
        assert!(!is_main_thread());
    }
}
