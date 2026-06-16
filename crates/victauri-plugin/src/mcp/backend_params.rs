use schemars::JsonSchema;
use serde::Deserialize;

/// Deserialize `args` as a JSON object (or absent), rejecting scalars/arrays. The documented
/// contract is that `args` is an object of `{parameter_name: value}`; forwarding a scalar or
/// array to `__TAURI_INTERNALS__.invoke` would break the handler's argument expectations, so we
/// reject it with a clear error at the boundary instead of letting it slip through.
fn deserialize_optional_object<'de, D>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Object(_)) => Ok(value),
        Some(_) => Err(serde::de::Error::custom(
            "`args` must be a JSON object of {parameter_name: value} (got a scalar or array)",
        )),
    }
}

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
    #[serde(default, deserialize_with = "deserialize_optional_object")]
    pub args: Option<serde_json::Value>,
    /// Target webview label.
    #[serde(alias = "window", alias = "window_label")]
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
///
/// The fields are consumed by the `sqlite`-gated query implementation; without that
/// feature `query_db` is a stub that returns a clear error, so the fields are accepted
/// (for a stable schema) but not read.
#[derive(Debug, Deserialize, JsonSchema)]
#[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
pub struct QueryDbParams {
    /// Path to the `SQLite` database file, relative to the app data directory.
    /// If omitted, Victauri auto-discovers `SQLite` databases in the app data directory.
    pub path: Option<String>,
    /// SQL query to execute. Must be a SELECT/PRAGMA/EXPLAIN statement (read-only).
    ///
    /// Accepts `sql` as an alias — the intuitive name an agent reaches for first.
    /// Live-4DA dogfood (2026-06-16): passing `sql` returned an opaque HTTP 400 with
    /// no hint that the field is `query`; the alias removes that paper-cut.
    #[serde(alias = "sql")]
    pub query: String,
    /// Positional bind parameters for the query (e.g. `["value1", 42]`).
    pub params: Option<Vec<serde_json::Value>>,
    /// Maximum number of rows to return. Default: 100.
    pub max_rows: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Live-4DA dogfood (2026-06-16): `query_db` accepted only `query`; passing the intuitive
    // `sql` key 400'd with no hint. The `sql` alias removes that paper-cut.
    #[test]
    fn query_db_accepts_sql_alias() {
        for key in ["query", "sql"] {
            let json = format!(r#"{{"{key}":"SELECT 1"}}"#);
            let p: QueryDbParams = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("key `{key}` must deserialize: {e}"));
            assert_eq!(
                p.query, "SELECT 1",
                "key `{key}` must populate the query field"
            );
        }
    }
}
