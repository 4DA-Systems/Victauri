use tauri::{Manager, Runtime};
use victauri_core::WindowState;

/// Runtime-erased interface for webview and backend access, allowing the MCP
/// server to interact with Tauri windows and the application backend without
/// generic parameters.
pub trait WebviewBridge: Send + Sync {
    /// Execute JavaScript in the target webview (defaults to "main" or first visible window).
    ///
    /// # Errors
    ///
    /// Returns an error string if no matching window is found or the eval fails.
    fn eval_webview(&self, label: Option<&str>, script: &str) -> Result<(), String>;
    /// Retrieve the state of one or all windows (position, size, visibility, focus, URL).
    fn get_window_states(&self, label: Option<&str>) -> Vec<WindowState>;
    /// Return the labels of all open webview windows.
    fn list_window_labels(&self) -> Vec<String>;
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
            .or_else(|| windows.values().find(|w| w.is_visible().unwrap_or(false)))
            .or_else(|| windows.values().next())
            .ok_or_else(|| "no window available".to_string()),
    }
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
fn on_main<R, T, F>(app: &tauri::AppHandle<R>, what: &str, f: F) -> Result<T, String>
where
    R: Runtime,
    T: Send + 'static,
    F: FnOnce(&tauri::AppHandle<R>) -> T + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    let app_for_closure = app.clone();
    app.run_on_main_thread(move || {
        // Send only fails if the caller already timed out and dropped the receiver — ignore.
        let _ = tx.send(f(&app_for_closure));
    })
    .map_err(|e| format!("failed to dispatch {what} to the main thread: {e}"))?;
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .map_err(|e| format!("{what} did not complete on the main thread: {e}"))
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

                states.push(WindowState {
                    label: win_label.clone(),
                    title: window.title().unwrap_or_default(),
                    url: window.url().map(|u| u.to_string()).unwrap_or_default(),
                    visible: window.is_visible().unwrap_or(false),
                    focused: window.is_focused().unwrap_or(false),
                    maximized: window.is_maximized().unwrap_or(false),
                    minimized: window.is_minimized().unwrap_or(false),
                    fullscreen: window.is_fullscreen().unwrap_or(false),
                    position: (pos.x, pos.y),
                    size: (size.width, size.height),
                });
            }

            states
        })
        .unwrap_or_default()
    }

    fn list_window_labels(&self) -> Vec<String> {
        on_main(self, "list_window_labels", |app| {
            app.webview_windows().keys().cloned().collect()
        })
        .unwrap_or_default()
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
        win_focus(hwnd);
        win_send_text(text)
    }

    #[cfg(windows)]
    fn native_key(&self, label: Option<&str>, key: &str) -> Result<(), String> {
        let hwnd = self.get_native_handle(label)?;
        win_focus(hwnd);
        win_send_key(key)
    }

    #[cfg(windows)]
    fn native_click(&self, label: Option<&str>, x: f64, y: f64) -> Result<(), String> {
        let hwnd = self.get_native_handle(label)?;
        win_focus(hwnd);
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

/// Bring the target window to the foreground so input is routed to it, then
/// give the OS a brief moment to apply focus.
#[allow(unsafe_code)]
#[cfg(windows)]
fn win_focus(hwnd: isize) {
    use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
    // SAFETY: hwnd comes from Tauri's window handle; SetForegroundWindow is safe
    // to call with any HWND (returns false if it fails).
    unsafe {
        let _ = SetForegroundWindow(win_hwnd(hwnd));
    }
    std::thread::sleep(std::time::Duration::from_millis(40));
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
        SM_YVIRTUALSCREEN,
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
