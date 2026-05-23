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

impl<R: Runtime> WebviewBridge for tauri::AppHandle<R> {
    fn eval_webview(&self, label: Option<&str>, script: &str) -> Result<(), String> {
        let windows = self.webview_windows();
        let webview = find_window(&windows, label)?;
        webview.eval(script).map_err(|e| e.to_string())
    }

    fn get_window_states(&self, label: Option<&str>) -> Vec<WindowState> {
        let windows = self.webview_windows();
        let mut states = Vec::new();

        for (win_label, window) in &windows {
            if let Some(filter) = label
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
    }

    fn list_window_labels(&self) -> Vec<String> {
        self.webview_windows().keys().cloned().collect()
    }

    fn get_native_handle(&self, label: Option<&str>) -> Result<isize, String> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let windows = self.webview_windows();
        let _webview = find_window(&windows, label)?;
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
    }

    fn manage_window(&self, label: Option<&str>, action: &str) -> Result<String, String> {
        let windows = self.webview_windows();
        let window = find_window(&windows, label)?;

        match action {
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
            "not_always_on_top" => window.set_always_on_top(false).map_err(|e| e.to_string())?,
            _ => return Err(format!("unknown action: {action}")),
        }

        Ok(format!("{action} executed"))
    }

    fn resize_window(&self, label: Option<&str>, width: u32, height: u32) -> Result<(), String> {
        let windows = self.webview_windows();
        let window = find_window(&windows, label)?;

        window
            .set_size(tauri::LogicalSize::new(width, height))
            .map_err(|e| e.to_string())
    }

    fn move_window(&self, label: Option<&str>, x: i32, y: i32) -> Result<(), String> {
        let windows = self.webview_windows();
        let window = find_window(&windows, label)?;

        window
            .set_position(tauri::LogicalPosition::new(x, y))
            .map_err(|e| e.to_string())
    }

    fn set_window_title(&self, label: Option<&str>, title: &str) -> Result<(), String> {
        let windows = self.webview_windows();
        let window = find_window(&windows, label)?;

        window.set_title(title).map_err(|e| e.to_string())
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
        serde_json::json!({
            "identifier": config.identifier,
            "product_name": config.product_name,
            "version": config.version,
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
