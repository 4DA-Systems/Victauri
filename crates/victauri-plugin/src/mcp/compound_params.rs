use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

// ── Enums ──────────────────────────────────────────────────────────────────

/// Web storage type for browser storage operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StorageType {
    /// Browser localStorage (persistent across sessions).
    Local,
    /// Browser sessionStorage (cleared when tab closes).
    Session,
}

impl fmt::Display for StorageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => f.write_str("local"),
            Self::Session => f.write_str("session"),
        }
    }
}

/// Browser dialog type for dialog response configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DialogType {
    /// JavaScript `alert()` dialog.
    Alert,
    /// JavaScript `confirm()` dialog.
    Confirm,
    /// JavaScript `prompt()` dialog.
    Prompt,
}

impl DialogType {
    /// Returns the lowercase string for JS bridge consumption.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alert => "alert",
            Self::Confirm => "confirm",
            Self::Prompt => "prompt",
        }
    }
}

impl fmt::Display for DialogType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Action to take on a browser dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DialogAction {
    /// Accept the dialog (click OK/Yes).
    Accept,
    /// Dismiss the dialog (click Cancel/No).
    Dismiss,
}

impl DialogAction {
    /// Returns the lowercase string for JS bridge consumption.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Dismiss => "dismiss",
        }
    }
}

impl fmt::Display for DialogAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── interact ────────────────────────────────────────────────────────────────

/// Action for the compound `interact` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InteractAction {
    /// Click an element.
    Click,
    /// Double-click an element.
    DoubleClick,
    /// Hover over an element.
    Hover,
    /// Focus an element.
    Focus,
    /// Scroll an element into view.
    ScrollIntoView,
    /// Select an option in a `<select>` element.
    SelectOption,
}

impl fmt::Display for InteractAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Click => f.write_str("click"),
            Self::DoubleClick => f.write_str("double_click"),
            Self::Hover => f.write_str("hover"),
            Self::Focus => f.write_str("focus"),
            Self::ScrollIntoView => f.write_str("scroll_into_view"),
            Self::SelectOption => f.write_str("select_option"),
        }
    }
}

/// Parameters for the compound `interact` tool (click, hover, focus, scroll, select).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct InteractParams {
    /// Action to perform: click, `double_click`, hover, focus, `scroll_into_view`, `select_option`.
    pub action: InteractAction,
    /// Ref handle ID from a DOM snapshot (e.g. "e5"). Required for click, `double_click`, hover, focus, `select_option`.
    pub ref_id: Option<String>,
    /// Option values for `select_option` action.
    pub values: Option<Vec<String>>,
    /// Single option value for `select_option` (convenience alias for `values`).
    pub value: Option<String>,
    /// Horizontal scroll position (pixels). Used with `scroll_into_view` when `ref_id` is null.
    pub x: Option<f64>,
    /// Vertical scroll position (pixels). Used with `scroll_into_view` when `ref_id` is null.
    pub y: Option<f64>,
    /// If true, deliver a real OS mouse click (`isTrusted: true`) at the
    /// element's center instead of a synthetic DOM click (for `click`). Falls
    /// back with an error on platforms without native-input support. Currently
    /// implemented on Windows.
    pub trusted: Option<bool>,
    /// Target webview label.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

// ── input ───────────────────────────────────────────────────────────────────

/// Action for the compound `input` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputAction {
    /// Set an input element's value directly.
    Fill,
    /// Type text character-by-character.
    TypeText,
    /// Press a keyboard key.
    PressKey,
}

impl fmt::Display for InputAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fill => f.write_str("fill"),
            Self::TypeText => f.write_str("type_text"),
            Self::PressKey => f.write_str("press_key"),
        }
    }
}

/// Parameters for the compound `input` tool (fill, `type_text`, `press_key`).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct InputParams {
    /// Action to perform: fill, `type_text`, `press_key`.
    pub action: InputAction,
    /// Ref handle ID of the target element. Required for fill and `type_text`.
    pub ref_id: Option<String>,
    /// Value to set (for fill action).
    pub value: Option<String>,
    /// Text to type character-by-character (for `type_text` action).
    pub text: Option<String>,
    /// Key to press (for `press_key` action, e.g. "Enter", "Escape", "Tab", "`ArrowDown`").
    pub key: Option<String>,
    /// If true, deliver real OS keyboard input (`isTrusted: true`) instead of
    /// synthetic DOM events — for `type_text`/`press_key`. The target element is
    /// focused first (via `ref_id`). Falls back with an error on platforms
    /// without native-input support. Currently implemented on Windows.
    pub trusted: Option<bool>,
    /// Target webview label.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

// ── window ──────────────────────────────────────────────────────────────────

/// Action for the compound `window` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
    /// Get the current state of a window.
    GetState,
    /// List all window labels.
    List,
    /// Manage a window (minimize, maximize, close, etc.).
    Manage,
    /// Resize a window.
    Resize,
    /// Move a window to a new position.
    MoveTo,
    /// Set a window's title.
    SetTitle,
    /// Probe every window and report which ones Victauri can actually introspect
    /// (i.e. have a responding JS bridge) vs. which are blind — usually because
    /// the window's capability is missing `victauri:default`.
    Introspectability,
}

impl fmt::Display for WindowAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GetState => f.write_str("get_state"),
            Self::List => f.write_str("list"),
            Self::Manage => f.write_str("manage"),
            Self::Resize => f.write_str("resize"),
            Self::MoveTo => f.write_str("move_to"),
            Self::SetTitle => f.write_str("set_title"),
            Self::Introspectability => f.write_str("introspectability"),
        }
    }
}

/// Window management sub-action for the `manage` action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManageAction {
    /// Minimize the window.
    Minimize,
    /// Restore from minimized state.
    Unminimize,
    /// Maximize the window.
    Maximize,
    /// Restore from maximized state.
    Unmaximize,
    /// Close the window.
    Close,
    /// Focus the window.
    Focus,
    /// Show the window.
    Show,
    /// Hide the window.
    Hide,
    /// Enter fullscreen mode.
    Fullscreen,
    /// Exit fullscreen mode.
    Unfullscreen,
    /// Set the window to always be on top.
    AlwaysOnTop,
    /// Remove the always-on-top flag.
    NotAlwaysOnTop,
}

impl ManageAction {
    /// Returns the `snake_case` string for bridge consumption.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::Unminimize => "unminimize",
            Self::Maximize => "maximize",
            Self::Unmaximize => "unmaximize",
            Self::Close => "close",
            Self::Focus => "focus",
            Self::Show => "show",
            Self::Hide => "hide",
            Self::Fullscreen => "fullscreen",
            Self::Unfullscreen => "unfullscreen",
            Self::AlwaysOnTop => "always_on_top",
            Self::NotAlwaysOnTop => "not_always_on_top",
        }
    }
}

impl fmt::Display for ManageAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parameters for the compound `window` tool (`get_state`, list, manage, resize, move, title).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WindowParams {
    /// Action to perform: `get_state`, list, manage, resize, `move_to`, `set_title`.
    pub action: WindowAction,
    /// Target window label.
    pub label: Option<String>,
    /// Window management action (for manage): minimize, unminimize, maximize, unmaximize, close, focus, show, hide, fullscreen, unfullscreen, `always_on_top`, `not_always_on_top`.
    pub manage_action: Option<ManageAction>,
    /// Width in logical pixels (for resize).
    pub width: Option<u32>,
    /// Height in logical pixels (for resize).
    pub height: Option<u32>,
    /// X position in logical pixels (for `move_to`).
    pub x: Option<i32>,
    /// Y position in logical pixels (for `move_to`).
    pub y: Option<i32>,
    /// New window title (for `set_title`).
    pub title: Option<String>,
}

// ── storage ─────────────────────────────────────────────────────────────────

/// Action for the compound `storage` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StorageAction {
    /// Read a value from storage.
    Get,
    /// Write a value to storage.
    Set,
    /// Delete a value from storage.
    Delete,
    /// Get all cookies.
    GetCookies,
}

impl fmt::Display for StorageAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Get => f.write_str("get"),
            Self::Set => f.write_str("set"),
            Self::Delete => f.write_str("delete"),
            Self::GetCookies => f.write_str("get_cookies"),
        }
    }
}

/// Parameters for the compound `storage` tool (get, set, delete, `get_cookies`).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct StorageParams {
    /// Action to perform: get, set, delete, `get_cookies`.
    pub action: StorageAction,
    /// Storage type for get/set/delete. Defaults to local if omitted.
    pub storage_type: Option<StorageType>,
    /// Key to read, write, or delete.
    pub key: Option<String>,
    /// Value to store (for set action). Will be JSON-serialized if not a string.
    pub value: Option<serde_json::Value>,
    /// Target webview label.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

// ── navigate ────────────────────────────────────────────────────────────────

/// Action for the compound `navigate` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NavigateAction {
    /// Navigate to a URL.
    GoTo,
    /// Navigate back in browser history.
    GoBack,
    /// Get the navigation history log.
    GetHistory,
    /// Set an auto-response for browser dialogs.
    SetDialogResponse,
    /// Get the dialog event log.
    GetDialogLog,
}

impl fmt::Display for NavigateAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GoTo => f.write_str("go_to"),
            Self::GoBack => f.write_str("go_back"),
            Self::GetHistory => f.write_str("get_history"),
            Self::SetDialogResponse => f.write_str("set_dialog_response"),
            Self::GetDialogLog => f.write_str("get_dialog_log"),
        }
    }
}

/// Parameters for the compound `navigate` tool (`go_to`, `go_back`, history, dialogs).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NavigateParams {
    /// Action to perform: `go_to`, `go_back`, `get_history`, `set_dialog_response`, `get_dialog_log`.
    pub action: NavigateAction,
    /// URL to navigate to (for `go_to` action).
    pub url: Option<String>,
    /// Dialog type (for `set_dialog_response`).
    pub dialog_type: Option<DialogType>,
    /// Dialog action (for `set_dialog_response`).
    pub dialog_action: Option<DialogAction>,
    /// Response text for prompt dialogs (for `set_dialog_response`).
    pub text: Option<String>,
    /// Target webview label.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

// ── recording ───────────────────────────────────────────────────────────────

/// Action for the compound `recording` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecordingAction {
    /// Begin recording events.
    Start,
    /// Stop recording and return the session.
    Stop,
    /// Save a state checkpoint.
    Checkpoint,
    /// List all checkpoints in the current session.
    ListCheckpoints,
    /// Get events since an index.
    GetEvents,
    /// Get events between two checkpoints.
    EventsBetween,
    /// Get an IPC replay sequence.
    GetReplay,
    /// Export the current session as JSON.
    Export,
    /// Import a previously exported session.
    Import,
    /// Replay recorded IPC commands and compare responses to baseline.
    Replay,
    /// Immediately drain pending bridge events into the recording (no 1-second wait).
    Flush,
}

impl fmt::Display for RecordingAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start => f.write_str("start"),
            Self::Stop => f.write_str("stop"),
            Self::Checkpoint => f.write_str("checkpoint"),
            Self::ListCheckpoints => f.write_str("list_checkpoints"),
            Self::GetEvents => f.write_str("get_events"),
            Self::EventsBetween => f.write_str("events_between"),
            Self::GetReplay => f.write_str("get_replay"),
            Self::Export => f.write_str("export"),
            Self::Import => f.write_str("import"),
            Self::Replay => f.write_str("replay"),
            Self::Flush => f.write_str("flush"),
        }
    }
}

/// Parameters for the compound `recording` tool (start, stop, checkpoint, replay, export, import).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecordingParams {
    /// Action to perform: start, stop, checkpoint, `list_checkpoints`, `get_events`, `events_between`, `get_replay`, export, import, replay, flush.
    pub action: RecordingAction,
    /// Session ID (for start — optional, UUID generated if omitted).
    pub session_id: Option<String>,
    /// Checkpoint ID (for checkpoint — optional, a `cp-<uuid>` is generated and
    /// returned if omitted).
    pub checkpoint_id: Option<String>,
    /// Checkpoint label (for checkpoint, optional). Also accepts `label` as an alias.
    #[serde(alias = "label")]
    pub checkpoint_label: Option<String>,
    /// State snapshot as JSON (for checkpoint).
    pub state: Option<serde_json::Value>,
    /// Starting checkpoint ID (for `events_between`).
    pub from: Option<String>,
    /// Ending checkpoint ID (for `events_between`).
    pub to: Option<String>,
    /// Only return events after this index (for `get_events`).
    pub since_index: Option<usize>,
    /// JSON string of a previously exported `RecordedSession` (for import).
    pub session_json: Option<String>,
    /// Target webview label (for replay).
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

// ── inspect ─────────────────────────────────────────────────────────────────

/// Action for the compound `inspect` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InspectAction {
    /// Get computed CSS styles for an element.
    GetStyles,
    /// Get bounding boxes for elements.
    GetBoundingBoxes,
    /// Add a debug highlight overlay to an element.
    Highlight,
    /// Remove all debug highlight overlays.
    ClearHighlights,
    /// Run an accessibility audit.
    AuditAccessibility,
    /// Get performance metrics (timing, heap, DOM).
    GetPerformance,
}

impl fmt::Display for InspectAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GetStyles => f.write_str("get_styles"),
            Self::GetBoundingBoxes => f.write_str("get_bounding_boxes"),
            Self::Highlight => f.write_str("highlight"),
            Self::ClearHighlights => f.write_str("clear_highlights"),
            Self::AuditAccessibility => f.write_str("audit_accessibility"),
            Self::GetPerformance => f.write_str("get_performance"),
        }
    }
}

/// Parameters for the compound `inspect` tool (styles, bounding boxes, highlight, a11y, perf).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct InspectParams {
    /// Action to perform: `get_styles`, `get_bounding_boxes`, highlight, `clear_highlights`, `audit_accessibility`, `get_performance`.
    pub action: InspectAction,
    /// Ref handle ID (for `get_styles`, highlight).
    pub ref_id: Option<String>,
    /// List of ref handle IDs (for `get_bounding_boxes`).
    pub ref_ids: Option<Vec<String>>,
    /// CSS property names to return (for `get_styles` — if omitted, returns key properties).
    pub properties: Option<Vec<String>>,
    /// CSS color for the highlight overlay (for highlight, default: "rgba(255, 0, 0, 0.3)").
    pub color: Option<String>,
    /// Text label displayed above the highlight (for highlight).
    #[serde(rename = "highlight_label")]
    pub label: Option<String>,
    /// Target webview label.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

// ── css ─────────────────────────────────────────────────────────────────────

/// Action for the compound `css` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CssAction {
    /// Inject custom CSS into the page.
    Inject,
    /// Remove previously injected CSS.
    Remove,
}

impl fmt::Display for CssAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inject => f.write_str("inject"),
            Self::Remove => f.write_str("remove"),
        }
    }
}

/// Parameters for the compound `css` tool (inject, remove).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CssParams {
    /// Action to perform: inject, remove.
    pub action: CssAction,
    /// CSS text to inject (for inject action).
    pub css: Option<String>,
    /// Allow remote references (`@import`, remote `url(...)`) in injected CSS. Default
    /// false: remote refs are blocked because they turn `css inject` into a data-exfil /
    /// SSRF vector (especially when chained with page-sourced prompt injection). Set true
    /// only when intentionally loading a remote stylesheet/asset for debugging.
    #[serde(default)]
    pub allow_remote: bool,
    /// Target webview label.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

// ── route (network interception) ──────────────────────────────────────────

/// Action for the compound `route` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RouteAction {
    /// Add a network route rule.
    Add,
    /// List active route rules.
    List,
    /// Remove a route rule by id.
    Clear,
    /// Remove all route rules.
    ClearAll,
    /// Return the log of intercepted requests.
    Matches,
}

impl fmt::Display for RouteAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Add => f.write_str("add"),
            Self::List => f.write_str("list"),
            Self::Clear => f.write_str("clear"),
            Self::ClearAll => f.write_str("clear_all"),
            Self::Matches => f.write_str("matches"),
        }
    }
}

/// How a route rule's pattern is matched against the request URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RouteMatchType {
    /// URL contains the pattern (default).
    Substring,
    /// Glob with `*` wildcards.
    Glob,
    /// JavaScript regular expression.
    Regex,
    /// Exact URL match.
    Exact,
}

impl RouteMatchType {
    /// Bridge string for this match type.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Substring => "substring",
            Self::Glob => "glob",
            Self::Regex => "regex",
            Self::Exact => "exact",
        }
    }
}

/// What a matched route rule does to the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RouteBehavior {
    /// Abort the request (the app sees a network failure).
    Block,
    /// Return a synthetic mock response (fetch only; XHR falls back to delay).
    Fulfill,
    /// Let the request proceed, but after `delay_ms` (latency injection).
    Delay,
}

impl RouteBehavior {
    /// Bridge string for this behavior.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Fulfill => "fulfill",
            Self::Delay => "delay",
        }
    }
}

/// Parameters for the compound `route` tool (network interception / mock / block / delay).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RouteParams {
    /// Action: add, list, clear, `clear_all`, matches.
    pub action: RouteAction,
    /// URL pattern to match (for add). Interpreted per `match_type`.
    pub pattern: Option<String>,
    /// How `pattern` is matched: substring (default), glob, regex, exact.
    pub match_type: Option<RouteMatchType>,
    /// Restrict to a single HTTP method (for add, optional).
    pub method: Option<String>,
    /// What the rule does: block, fulfill, delay (for add). Defaults to fulfill.
    pub behavior: Option<RouteBehavior>,
    /// Mock response status code (for fulfill). Default 200.
    pub status: Option<u16>,
    /// Mock response status text (for fulfill).
    pub status_text: Option<String>,
    /// Mock response headers as a JSON object (for fulfill).
    pub headers: Option<serde_json::Value>,
    /// Mock response body (for fulfill). Strings sent as-is; other JSON is serialized.
    pub body: Option<serde_json::Value>,
    /// Mock response content-type (for fulfill). Default "application/json".
    pub content_type: Option<String>,
    /// Delay in milliseconds (for delay, or to delay a fulfill).
    pub delay_ms: Option<u64>,
    /// Maximum times this rule fires (for add). 0 or omitted = unlimited.
    pub times: Option<u64>,
    /// Route rule id (for clear).
    pub id: Option<u64>,
    /// Maximum match-log entries to return (for matches).
    pub limit: Option<usize>,
    /// Target webview label.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

// ── trace (screencast) ──────────────────────────────────────────────────────

/// Action for the compound `trace` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TraceAction {
    /// Start capturing screenshots at a fixed interval.
    Start,
    /// Stop capturing and return a summary.
    Stop,
    /// Report whether a trace is active and how many frames are buffered.
    Status,
    /// Return captured frames (base64 PNGs).
    Frames,
}

impl fmt::Display for TraceAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start => f.write_str("start"),
            Self::Stop => f.write_str("stop"),
            Self::Status => f.write_str("status"),
            Self::Frames => f.write_str("frames"),
        }
    }
}

/// Parameters for the compound `trace` tool (screencast / visual timeline).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TraceParams {
    /// Action: start, stop, status, frames.
    pub action: TraceAction,
    /// Capture interval in milliseconds (for start). Default 500, min 50.
    pub interval_ms: Option<u64>,
    /// Maximum frames to retain in the ring buffer (for start). Default 60, max 600.
    pub max_frames: Option<usize>,
    /// If true (for start), also start the event recorder so the trace bundles
    /// the IPC/DOM/console event timeline alongside the screencast.
    pub with_events: Option<bool>,
    /// Maximum frames to return (for frames). 0 or omitted returns all buffered.
    pub limit: Option<usize>,
    /// Target webview label to capture.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

// ── animation ─────────────────────────────────────────────────────────────

/// Action for the compound `animation` tool (motion introspection / scrubbing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnimationAction {
    /// List running CSS animations/transitions with timing, easing, and keyframes.
    List,
    /// Deterministically pause + seek the target's animation to N points,
    /// returning the geometry curve and (optionally) a contact-sheet filmstrip.
    Scrub,
    /// Real-time motion + jank recorder. `record=true` arms a rAF watcher;
    /// `record=false` reads back the measured curve and dropped-frame stats.
    Sample,
}

impl fmt::Display for AnimationAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::List => f.write_str("list"),
            Self::Scrub => f.write_str("scrub"),
            Self::Sample => f.write_str("sample"),
        }
    }
}

/// Parameters for the compound `animation` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AnimationParams {
    /// Action to perform: `list`, `scrub`.
    pub action: AnimationAction,
    /// CSS selector for the target element. For `list`, scopes the query (omit
    /// for all running animations). For `scrub`, selects the element to seek
    /// (omit to auto-pick the first currently-animating element).
    pub selector: Option<String>,
    /// (`scrub`) Number of evenly-spaced progress points to sample. Default 20,
    /// clamped to 2..=120.
    pub points: Option<usize>,
    /// (`scrub`) If true, capture a native screenshot at each point and return a
    /// single contact-sheet filmstrip PNG. Default false (geometry curve only).
    pub capture: Option<bool>,
    /// (`scrub`) If true (default), resume the animation after scrubbing;
    /// otherwise leave it paused at the final point.
    pub restore: Option<bool>,
    /// (`scrub`, with `capture`) Columns in the filmstrip grid. Default ~sqrt(n).
    pub cols: Option<usize>,
    /// (`sample`) If true, arm the rAF recorder; if false (default), read back
    /// recorded sessions.
    pub record: Option<bool>,
    /// (`sample`, read) If true, clear recorded sessions after returning them.
    pub clear: Option<bool>,
    /// Target webview label.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

// ── logs ────────────────────────────────────────────────────────────────────

/// Action for the compound `logs` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogsAction {
    /// Get captured console.log/warn/error entries.
    Console,
    /// Get intercepted fetch/XHR network requests.
    Network,
    /// Get IPC call log.
    Ipc,
    /// Get URL change history.
    Navigation,
    /// Get alert/confirm/prompt dialog events.
    Dialogs,
    /// Get combined event stream.
    Events,
    /// Find slow IPC calls exceeding a threshold.
    SlowIpc,
    /// Clear the IPC + network logs (per-test isolation — start a clean window
    /// before exercising the app so `detect_ghost_commands`/`logs ipc` reflect
    /// only the current test's traffic, not stale accumulated probe history).
    Clear,
}

impl fmt::Display for LogsAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Console => f.write_str("console"),
            Self::Network => f.write_str("network"),
            Self::Ipc => f.write_str("ipc"),
            Self::Navigation => f.write_str("navigation"),
            Self::Dialogs => f.write_str("dialogs"),
            Self::Events => f.write_str("events"),
            Self::SlowIpc => f.write_str("slow_ipc"),
            Self::Clear => f.write_str("clear"),
        }
    }
}

/// Parameters for the compound `logs` tool (console, network, ipc, navigation, dialogs, events).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LogsParams {
    /// Action to perform: console, network, ipc, navigation, dialogs, events, `slow_ipc`.
    pub action: LogsAction,
    /// Only return entries after this Unix timestamp in milliseconds (for console, events).
    pub since: Option<f64>,
    /// Filter by URL substring (for network).
    pub filter: Option<String>,
    /// Maximum number of entries to return (for ipc, network, `slow_ipc`).
    pub limit: Option<usize>,
    /// Threshold in milliseconds for slow IPC calls (for `slow_ipc`).
    pub threshold_ms: Option<u64>,
    /// When true (for ipc action), await up to 500ms for the latest IPC entry's
    /// response body to be fully captured. Uses event-driven signaling from the
    /// fetch interceptor rather than polling.
    pub wait_for_capture: Option<bool>,
    /// Target webview label.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}
