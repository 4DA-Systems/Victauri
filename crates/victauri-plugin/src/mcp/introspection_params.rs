use schemars::JsonSchema;
use serde::Deserialize;

/// Actions available in the `introspect` compound tool.
#[derive(Debug, Copy, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IntrospectAction {
    /// Get per-command execution timing statistics (min/max/avg/p95). Returns both
    /// `commands` (Victauri-driven `invoke_command` calls) and `ipc_traffic` (the app's
    /// REAL frontend IPC, derived from the live IPC log — the one reflecting actual use).
    CommandTimings,
    /// Report which registered commands have been called during this session. Also
    /// returns `ipc_calls_observed` and `invoked_not_registered` from the live IPC log.
    Coverage,
    /// Record the current response shape of a command as a baseline contract.
    ContractRecord,
    /// Check all recorded contracts for schema drift.
    ContractCheck,
    /// List all recorded contract baselines.
    ContractList,
    /// Clear all recorded contract baselines.
    ContractClear,
    /// Report Victauri plugin startup phase timing breakdown.
    StartupTiming,
    /// Enumerate Tauri v2 capabilities, security config, plugin config, and window definitions.
    Capabilities,
    /// `SQLite` database health diagnostics (journal mode, WAL, page stats).
    DbHealth,
    /// Snapshot of the Victauri plugin's internal state (event log, registry, recording, faults).
    PluginState,
    /// Enumerate the host process and its child processes (sidecars, background workers).
    Processes,
    /// Report Victauri's own spawned async tasks (MCP server, event drain) and their status.
    PluginTasks,
    /// List captured Tauri event bus events (automatically intercepted).
    EventBus,
    /// Clear the event bus capture buffer.
    EventBusClear,
    /// Build a command catalog by mining the live IPC log for each command's argument
    /// and result *shapes*, merged with the `#[inspectable]` registry. Gives an agent
    /// real call/return schemas for an app's commands even when the app does not use
    /// `#[inspectable]` (the registry would otherwise be names-only).
    ///
    /// NOTE: kept LAST so adding it does not shift the discriminants of the existing
    /// variants (serde matches by name, but a mid-enum insert is a needless semver break).
    CommandCatalog,
}

/// Parameters for the `introspect` compound tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct IntrospectParams {
    /// Which introspection action to perform.
    pub action: IntrospectAction,

    /// For `command_timings`: only show commands slower than this threshold (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slow_threshold_ms: Option<f64>,

    /// For `contract_record`: the command to record a baseline for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// For `contract_record`: optional arguments to pass when invoking the command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,

    /// For `db_health`: path to the `SQLite` database file. Read only by the
    /// `sqlite`-gated db-health impl; accepted (and ignored) without that feature.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    pub db_path: Option<String>,

    /// Target webview for actions that need JS eval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webview_label: Option<String>,
    // NB: `event_bus` options (`limit`, `since_ms`) are read from the generic `args` object
    // rather than dedicated fields — adding public fields to this externally-constructible
    // struct would be a semver-major break (it must stay additive within ^0.7).
}

/// Actions available in the `fault` compound tool.
#[derive(Debug, Copy, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FaultAction {
    /// Inject a fault rule for a specific command.
    Inject,
    /// List all active fault injection rules.
    List,
    /// Remove a specific fault rule.
    Clear,
    /// Remove all fault rules.
    ClearAll,
}

/// The type of fault to inject.
#[derive(Debug, Copy, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FaultKind {
    /// Add artificial delay before command execution.
    Delay,
    /// Return an error without executing the command.
    Error,
    /// Drop the response (return empty result).
    Drop,
    /// Execute normally but corrupt the response structure.
    Corrupt,
}

/// Parameters for the `fault` compound tool.
///
/// SCOPE (important): injected faults apply ONLY to commands executed through
/// Victauri's own `invoke_command` tool. They do NOT intercept the application's
/// real frontend-driven IPC (`window.__TAURI_INTERNALS__.invoke` → Tauri's native
/// transport), which runs below the JS layer Victauri can reach. Use `fault` to
/// probe a backend handler's behavior under failure when YOU drive the command
/// (e.g. "does my error path return the right shape on a DB error?"). It does not
/// reproduce a failure a user clicking the UI would experience — that path is not
/// interceptable cross-platform without CDP.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FaultParams {
    /// Which fault action to perform.
    pub action: FaultAction,

    /// Target command name (required for `inject` and `clear`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Type of fault to inject (required for `inject`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fault_type: Option<FaultKind>,

    /// For `delay` faults: delay in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,

    /// For `error` faults: error message to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,

    /// Maximum number of times to trigger (0 or omit for unlimited).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_triggers: Option<u64>,
}

/// Actions available in the `explain` compound tool.
#[derive(Debug, Copy, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExplainAction {
    /// Summarize recent activity across all layers (IPC, DOM, console, network, window events).
    Summary,
    /// Correlate the most recent burst of activity into a causal timeline.
    LastAction,
    /// Report what changed in the last N seconds (events, IPC calls, console entries).
    Diff,
}

/// Parameters for the `explain` compound tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExplainParams {
    /// Which explain action to perform.
    pub action: ExplainAction,

    /// How many seconds to look back (default: 30 for summary, 5 for `last_action`, 10 for diff).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
}
