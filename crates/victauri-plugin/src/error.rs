//! Typed error enums for the `victauri-plugin` crate.

/// Errors that can occur when building the Victauri plugin via [`crate::VictauriBuilder`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuilderError {
    /// The configured port is invalid (e.g., port 0).
    #[error("invalid port {port}: {reason}")]
    InvalidPort {
        /// The invalid port number.
        port: u16,
        /// Why the port is invalid.
        reason: String,
    },

    /// The event log capacity is out of the valid range.
    #[error("invalid event capacity {capacity}: {reason}")]
    InvalidEventCapacity {
        /// The invalid capacity value.
        capacity: usize,
        /// Why the capacity is invalid.
        reason: String,
    },

    /// The recorder capacity is out of the valid range.
    #[error("invalid recorder capacity {capacity}: {reason}")]
    InvalidRecorderCapacity {
        /// The invalid capacity value.
        capacity: usize,
        /// Why the capacity is invalid.
        reason: String,
    },

    /// The eval timeout is out of the valid range.
    #[error("invalid eval timeout {timeout_secs}s: {reason}")]
    InvalidEvalTimeout {
        /// The invalid timeout in seconds.
        timeout_secs: u64,
        /// Why the timeout is invalid.
        reason: String,
    },

    /// A custom redaction pattern does not compile.
    #[error("invalid redaction pattern '{pattern}': {reason}")]
    InvalidRedactionPattern {
        /// The pattern that failed.
        pattern: String,
        /// The regex compiler's error.
        reason: String,
    },
}

/// Errors that can occur during MCP server operation, tool execution, or webview interaction.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginError {
    /// A JavaScript eval timed out waiting for a response from the webview.
    #[error("eval timed out after {timeout_secs}s")]
    EvalTimeout {
        /// How many seconds elapsed before the timeout.
        timeout_secs: u64,
    },

    /// A JavaScript eval returned an error from the webview.
    #[error("eval failed: {message}")]
    EvalFailed {
        /// The error message from the webview.
        message: String,
    },

    /// Too many concurrent eval requests are pending.
    #[error("too many concurrent eval requests (limit: {limit})")]
    EvalConcurrencyExceeded {
        /// The maximum number of concurrent eval requests.
        limit: usize,
    },

    /// The webview bridge returned an error.
    #[error("bridge error: {message}")]
    BridgeError {
        /// The error message from the bridge.
        message: String,
    },

    /// Screenshot capture failed.
    #[error("screenshot failed: {message}")]
    ScreenshotFailed {
        /// The error message from the screenshot subsystem.
        message: String,
    },

    /// Bearer-token authentication failed.
    #[error("authentication failed: {message}")]
    AuthenticationFailed {
        /// The error message describing the auth failure.
        message: String,
    },

    /// The rate limiter rejected the request.
    #[error("rate limit exceeded")]
    RateLimitExceeded,

    /// The requested tool is disabled by the privacy configuration.
    #[error("tool '{tool_name}' is disabled by privacy configuration")]
    ToolDisabled {
        /// Name of the disabled tool.
        tool_name: String,
    },

    /// The requested command is blocked by the privacy configuration.
    #[error("command '{command}' is blocked by privacy configuration")]
    CommandBlocked {
        /// Name of the blocked command.
        command: String,
    },

    /// No window with the given label was found.
    #[error("window not found: {label}")]
    WindowNotFound {
        /// The window label that was not found.
        label: String,
    },

    /// The MCP server failed to start (e.g., all ports in use).
    #[error("MCP server failed to start: {message}")]
    ServerStartFailed {
        /// The error message from the server startup.
        message: String,
    },

    /// The configured port is already bound by another process.
    #[error("port {port} is already in use")]
    PortInUse {
        /// The port that is already in use.
        port: u16,
    },

    /// JSON serialization or deserialization failed.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// An I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A URL failed validation.
    #[error("invalid URL: {message}")]
    InvalidUrl {
        /// The error message describing why the URL is invalid.
        message: String,
    },

    /// An error propagated from the `victauri-core` crate.
    #[error(transparent)]
    Core(#[from] victauri_core::VictauriError),
}

/// Convenience alias for `std::result::Result<T, PluginError>`.
pub type Result<T> = std::result::Result<T, PluginError>;
