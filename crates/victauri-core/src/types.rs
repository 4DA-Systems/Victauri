//! Shared value types: ref handles, memory deltas, and verification results.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A short-lived handle to a DOM element, identified by a semantic ref rather than a CSS selector.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RefHandle {
    /// Unique ref identifier (e.g. "e3") assigned during DOM snapshot.
    pub id: String,
    /// CSS selector that can locate this element in the DOM.
    pub selector: String,
    /// ARIA role of the element, if present.
    pub role: Option<String>,
    /// Accessible name of the element, if present.
    pub name: Option<String>,
}

/// Memory usage delta measured before and after a command execution.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MemoryDelta {
    /// Allocated bytes before the command ran.
    pub before_bytes: i64,
    /// Allocated bytes after the command ran.
    pub after_bytes: i64,
    /// Net change in bytes (positive = growth).
    pub delta_bytes: i64,
    /// Name of the command that was measured.
    pub command: String,
}

/// Result of comparing frontend and backend state for cross-boundary verification.
///
/// # Examples
///
/// ```
/// use victauri_core::verify_state;
/// use serde_json::json;
///
/// let result = verify_state(json!({"count": 1}), json!({"count": 1}));
/// assert!(result.passed);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct VerificationResult {
    /// True if no divergences were found between frontend and backend state.
    pub passed: bool,
    /// State snapshot from the frontend (webview).
    pub frontend_state: serde_json::Value,
    /// State snapshot from the backend (Rust).
    pub backend_state: serde_json::Value,
    /// List of detected differences between frontend and backend.
    pub divergences: Vec<Divergence>,
}

/// A single mismatch between frontend and backend state at a specific JSON path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Divergence {
    /// Dot-separated JSON path where the mismatch occurs (e.g. "settings.theme").
    pub path: String,
    /// The value found in the frontend state.
    pub frontend_value: serde_json::Value,
    /// The value found in the backend state.
    pub backend_value: serde_json::Value,
    /// How serious this divergence is.
    pub severity: DivergenceSeverity,
}

/// Severity classification for a state divergence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DivergenceSeverity {
    /// Minor difference unlikely to affect correctness (e.g. floating-point rounding).
    Info,
    /// Potential issue such as a missing key or null value on one side.
    Warning,
    /// Definite mismatch between frontend and backend values.
    Error,
}

impl fmt::Display for DivergenceSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        };
        f.write_str(s)
    }
}

impl fmt::Display for Divergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {} \u{2260} {} ({})",
            self.path, self.frontend_value, self.backend_value, self.severity
        )
    }
}

/// # Examples
///
/// ```
/// use victauri_core::verify_state;
/// use serde_json::json;
///
/// let passed = verify_state(json!({}), json!({}));
/// assert_eq!(passed.to_string(), "verification passed");
///
/// let failed = verify_state(json!({"a": 1}), json!({"a": 2}));
/// assert_eq!(failed.to_string(), "verification failed: 1 divergence(s)");
/// ```
impl fmt::Display for VerificationResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.passed {
            f.write_str("verification passed")
        } else {
            let n = self.divergences.len();
            write!(f, "verification failed: {n} divergence(s)")
        }
    }
}
