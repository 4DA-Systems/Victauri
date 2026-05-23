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
    pub webview_label: Option<String>,
}

/// Parameters for the `detect_ghost_commands` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GhostCommandParams {
    /// Optional filter: only consider IPC calls from this webview label.
    pub webview_label: Option<String>,
}

/// Parameters for the `check_ipc_integrity` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct IpcIntegrityParams {
    /// Age in milliseconds after which a pending IPC call is considered stale. Default: 5000.
    pub stale_threshold_ms: Option<i64>,
    /// Target webview label.
    pub webview_label: Option<String>,
}
