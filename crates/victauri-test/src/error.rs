/// Errors that can occur when interacting with the Victauri MCP server from tests.
///
/// Each variant includes actionable context to help diagnose and fix the issue.
#[derive(Debug)]
#[non_exhaustive]
pub enum TestError {
    /// Failed to connect to the Victauri MCP server at the expected port.
    Connection {
        /// Host that was targeted (typically `"127.0.0.1"`).
        host: String,
        /// Port that was targeted.
        port: u16,
        /// Human-readable explanation of what went wrong.
        reason: String,
    },

    /// An HTTP-level error occurred during an MCP request.
    Request(reqwest::Error),

    /// The MCP server returned a JSON-RPC error response.
    Mcp {
        /// JSON-RPC error code.
        code: i64,
        /// Human-readable error message from the server.
        message: String,
    },

    /// A tool call returned `isError: true` in its result.
    ToolError(String),

    /// A test assertion evaluated to false.
    Assertion(String),

    /// A `wait_for` condition did not become true within the allowed time.
    Timeout(String),

    /// An element matching the given criteria was not found in the DOM snapshot.
    ElementNotFound(String),

    /// A visual regression was detected — screenshot differs from baseline.
    VisualRegression(String),

    /// A catch-all for errors that don't fit other variants (IO, encoding, etc.).
    Other(String),
}

impl From<reqwest::Error> for TestError {
    fn from(e: reqwest::Error) -> Self {
        Self::Request(e)
    }
}

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connection { host, port, reason } => {
                write!(
                    f,
                    "connection failed ({host}:{port}): {reason}\n\
                     \n  Possible fixes:\n\
                     \x20 - Is your Tauri app running? Start it with: pnpm tauri dev\n\
                     \x20 - Check that victauri-plugin is wired in your Tauri builder\n\
                     \x20 - Try a different port: VICTAURI_PORT={port} cargo test\n\
                     \x20 - Run `victauri doctor` for full diagnostics"
                )
            }
            Self::Request(e) => {
                write!(f, "MCP request failed: {e}")
            }
            Self::Mcp { code, message } => {
                let hint = match *code {
                    -32600 => "\n  Hint: invalid request — check your MCP protocol version",
                    -32601 => "\n  Hint: method not found — the tool may be disabled by privacy profile",
                    -32602 => "\n  Hint: invalid params — check the tool's expected arguments",
                    -32603 => "\n  Hint: internal error — check the Tauri app's stderr for details",
                    _ => "",
                };
                write!(f, "MCP error {code}: {message}{hint}")
            }
            Self::ToolError(msg) => write!(f, "tool call failed: {msg}"),
            Self::Assertion(msg) => write!(f, "assertion failed: {msg}"),
            Self::Timeout(msg) => {
                write!(
                    f,
                    "timeout: {msg}\n\
                     \n  Possible fixes:\n\
                     \x20 - Increase timeout: .timeout_ms(10_000) or wait_for(..., Some(15_000), ...)\n\
                     \x20 - Check that the expected condition can actually be met\n\
                     \x20 - Look for JS errors: client.get_console_logs().await"
                )
            }
            Self::ElementNotFound(msg) => {
                write!(
                    f,
                    "element not found: {msg}\n\
                     \n  Possible fixes:\n\
                     \x20 - Take a DOM snapshot to see what's on the page: client.dom_snapshot().await\n\
                     \x20 - The element may not have rendered yet — use expect().to_be_visible().await\n\
                     \x20 - Check for typos in the locator query"
                )
            }
            Self::VisualRegression(msg) => {
                write!(
                    f,
                    "visual regression: {msg}\n\
                     \n  If the change is intentional, delete the baseline image to regenerate it.\n\
                     \x20 Use ThresholdPreset::Relaxed for cross-platform tolerance."
                )
            }
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for TestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Request(e) => Some(e),
            _ => None,
        }
    }
}
