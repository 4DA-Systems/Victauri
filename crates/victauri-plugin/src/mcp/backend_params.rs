use schemars::JsonSchema;
use serde::Deserialize;

/// Parameters for the `get_registry` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RegistryParams {
    /// Search query to filter commands by name or description.
    pub query: Option<String>,
}

/// Parameters for the `invoke_command` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct InvokeCommandParams {
    /// The Tauri command name to invoke (e.g. "greet", "`save_settings`").
    pub command: String,
    /// Command arguments as a JSON OBJECT nested under this `args` key — keys are the Tauri
    /// command's parameter names, e.g. `{"command":"get_item","args":{"itemId":42}}`. Do NOT
    /// put parameters at the top level next to `command` (a flat `{"command":...,"itemId":42}`
    /// leaves `args` empty and the handler sees a missing argument). Omit for no-arg commands.
    /// Forwarded verbatim to `__TAURI_INTERNALS__.invoke(command, args)` — identical via the
    /// MCP tool and the REST `POST /api/tools/invoke_command` endpoint.
    pub args: Option<serde_json::Value>,
    /// Target webview label.
    pub webview_label: Option<String>,
}

/// Which app directory to target.
#[derive(Debug, Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum AppDir {
    /// Per-user app data directory.
    Data,
    /// Per-user config directory.
    Config,
    /// Log directory.
    Log,
    /// Local data directory.
    LocalData,
}

/// Parameters for the `list_app_dir` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListAppDirParams {
    /// Which app directory to list. Default: data.
    pub directory: Option<AppDir>,
    /// Optional subdirectory path relative to the chosen root (e.g. "databases").
    pub path: Option<String>,
    /// Only return entries matching this glob pattern (e.g. "*.sqlite", "*.db").
    pub pattern: Option<String>,
    /// Maximum directory depth to recurse. Default: 1 (immediate children only).
    pub max_depth: Option<u32>,
}

/// Parameters for the `read_app_file` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadAppFileParams {
    /// Which app directory the file is relative to. Default: data.
    pub directory: Option<AppDir>,
    /// File path relative to the chosen directory (e.g. "settings.json", "databases/app.db").
    pub path: String,
    /// Maximum number of bytes to read. Default: 1MB. Set lower for large files.
    pub max_bytes: Option<usize>,
    /// If true, return raw base64-encoded bytes instead of UTF-8 text.
    pub binary: Option<bool>,
}

/// Parameters for the `query_db` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct QueryDbParams {
    /// Path to the `SQLite` database file, relative to the app data directory.
    /// If omitted, Victauri auto-discovers `SQLite` databases in the app data directory.
    pub path: Option<String>,
    /// SQL query to execute. Must be a SELECT/PRAGMA/EXPLAIN statement (read-only).
    pub query: String,
    /// Positional bind parameters for the query (e.g. `["value1", 42]`).
    pub params: Option<Vec<serde_json::Value>>,
    /// Maximum number of rows to return. Default: 100.
    pub max_rows: Option<usize>,
}
