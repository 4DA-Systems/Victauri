use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

// ── Enums ──────────────────────────────────────────────────────────────────

/// Condition to poll for in the `wait_for` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WaitCondition {
    /// Wait for text to appear in the page.
    Text,
    /// Wait for text to disappear from the page.
    TextGone,
    /// Wait for a CSS selector to match an element.
    Selector,
    /// Wait for a CSS selector to stop matching.
    SelectorGone,
    /// Wait for the URL to contain a substring.
    Url,
    /// Wait for all IPC calls to complete.
    IpcIdle,
    /// Wait for all network requests to complete.
    NetworkIdle,
}

impl WaitCondition {
    /// Returns the `snake_case` string for JS bridge consumption.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::TextGone => "text_gone",
            Self::Selector => "selector",
            Self::SelectorGone => "selector_gone",
            Self::Url => "url",
            Self::IpcIdle => "ipc_idle",
            Self::NetworkIdle => "network_idle",
        }
    }
}

impl fmt::Display for WaitCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── Intent ─────────────────────────────────────────────────────────────────

/// Parameters for the `resolve_command` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResolveCommandParams {
    /// Natural language query describing what you want to do (e.g. "save the user's settings").
    pub query: String,
    /// Maximum number of results to return. Default: 5.
    pub limit: Option<usize>,
}

/// Parameters for the `semantic_assert` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SemanticAssertParams {
    /// JavaScript expression to evaluate in the webview. The result is checked against the assertion.
    pub expression: String,
    /// Human-readable label for this assertion (e.g. "user is logged in").
    pub label: String,
    /// Condition to evaluate against the actual value.
    pub condition: victauri_core::AssertionCondition,
    /// Expected value for the assertion.
    pub expected: serde_json::Value,
    /// Target webview label.
    pub webview_label: Option<String>,
}

// ── Wait ───────────────────────────────────────────────────────────────────

/// Parameters for the `wait_for` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitForParams {
    /// Condition to wait for.
    pub condition: WaitCondition,
    /// Value for the condition (text to find, CSS selector, URL substring).
    pub value: Option<String>,
    /// Maximum time to wait in milliseconds. Default: 10000.
    pub timeout_ms: Option<u64>,
    /// Polling interval in milliseconds. Default: 200.
    pub poll_ms: Option<u64>,
    /// Target webview label.
    pub webview_label: Option<String>,
}

// ── Find Elements ──────────────────────────────────────────────────────────

/// Parameters for the `find_elements` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindElementsParams {
    /// Text content to search for (case-insensitive substring match).
    pub text: Option<String>,
    /// ARIA role to match (exact match).
    pub role: Option<String>,
    /// data-testid attribute value to match (exact match).
    pub test_id: Option<String>,
    /// CSS selector to match.
    pub css: Option<String>,
    /// Accessible name to search for (aria-label, title, placeholder -- case-insensitive substring).
    pub name: Option<String>,
    /// Maximum number of results to return. Default: 10.
    pub max_results: Option<u32>,
    /// HTML tag name to match (e.g. "button", "input").
    pub tag: Option<String>,
    /// Placeholder text to match (case-insensitive substring).
    pub placeholder: Option<String>,
    /// Alt text to match (case-insensitive substring).
    pub alt: Option<String>,
    /// Title attribute to match (case-insensitive substring).
    pub title_attr: Option<String>,
    /// Associated label text to match (finds inputs by their label).
    pub label: Option<String>,
    /// If true, text matching is exact instead of substring.
    pub exact: Option<bool>,
    /// Filter by enabled state.
    pub enabled: Option<bool>,
    /// Target webview label.
    pub webview_label: Option<String>,
}
