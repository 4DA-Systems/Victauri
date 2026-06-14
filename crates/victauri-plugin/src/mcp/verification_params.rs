use schemars::JsonSchema;
use serde::Deserialize;

/// Parameters for the `verify_state` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct VerifyStateParams {
    /// JavaScript expression that returns the frontend state object to compare.
    pub frontend_expr: String,
    /// Backend state as a JSON object to compare against.
    /// Either this or `backend_command` must be provided.
    pub backend_state: Option<serde_json::Value>,
    /// Tauri command to invoke to fetch backend state for comparison.
    /// The command result is used as the backend state. Mutually exclusive with `backend_state`.
    pub backend_command: Option<String>,
    /// Arguments for the `backend_command`, if any.
    pub backend_args: Option<serde_json::Value>,
    /// Target webview label.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

/// Parameters for the `detect_ghost_commands` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GhostCommandParams {
    /// Optional filter: only consider IPC calls from this webview label.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
    /// Optional time window in milliseconds. When set (> 0), only commands invoked
    /// within the last `since_ms` ms are considered — a non-destructive way to scope
    /// detection to the current test's traffic without clearing the session's IPC log
    /// (the alternative, `logs {action:'clear'}`, wipes it for every reader). Pair with
    /// invoking the suspect action immediately before this call. Omit to scan the whole
    /// accumulated log.
    pub since_ms: Option<i64>,
}

/// Parameters for the `check_ipc_integrity` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct IpcIntegrityParams {
    /// Age in milliseconds after which a pending IPC call is considered stale. Default: 5000.
    pub stale_threshold_ms: Option<i64>,
    /// Target webview label.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}
