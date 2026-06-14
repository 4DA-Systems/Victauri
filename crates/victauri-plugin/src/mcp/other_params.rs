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
    /// Poll a JavaScript expression until it is truthy (or equals `expected`).
    ///
    /// The expression is evaluated in the webview each poll via the same engine
    /// as `eval_js`, so it may `await` (e.g.
    /// `(await window.__TAURI_INTERNALS__.invoke('get_status')).running === false`).
    /// This is the level-triggered, race-free way to await a fire-and-forget
    /// backend command that exposes a pollable status — no app changes required.
    Expression,
    /// Block until a named Tauri event fires on the app's event bus.
    ///
    /// Edge-triggered completion: evaluated server-side against Victauri's
    /// captured event bus, with a `since_ms` look-back so an event that fired in
    /// the gap between `invoke_command` and this call is not missed. The app must
    /// emit the event and Victauri must be configured to capture it via
    /// `VictauriBuilder::listen_events(&["..."])` (window-lifecycle events are
    /// captured automatically).
    Event,
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
            Self::Expression => "expression",
            Self::Event => "event",
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
    /// Optional — defaults to empty so a minimal `{expression, condition}` call
    /// succeeds instead of failing deserialization with an opaque 400.
    #[serde(default)]
    pub label: String,
    /// Condition to evaluate against the actual value.
    pub condition: victauri_core::AssertionCondition,
    /// Expected value for the assertion. Optional for truthy/falsy/exists conditions.
    #[serde(default)]
    pub expected: serde_json::Value,
    /// Target webview label.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

// ── Wait ───────────────────────────────────────────────────────────────────

/// Parameters for the `wait_for` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitForParams {
    /// Condition to wait for.
    pub condition: WaitCondition,
    /// Value for the condition: text to find, CSS selector, URL substring,
    /// JS expression (for `expression`), or Tauri event name (for `event`).
    pub value: Option<String>,
    /// Maximum time to wait in milliseconds. Default: 10000.
    pub timeout_ms: Option<u64>,
    /// Polling interval in milliseconds. Default: 200.
    pub poll_ms: Option<u64>,
    /// For the `expression` condition: the JSON value the expression must equal
    /// to satisfy the wait. When omitted, the wait is satisfied as soon as the
    /// expression evaluates to a truthy value.
    #[serde(default)]
    pub expected: Option<serde_json::Value>,
    /// For the `event` condition: how far back (in milliseconds) to look for a
    /// matching event when the wait begins, so an event that fired just before
    /// this call is not missed. Default: 2000.
    pub since_ms: Option<u64>,
    /// Target webview label.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

// ── App State Probes ─────────────────────────────────────────────────────────

/// Parameters for the `app_state` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AppStateParams {
    /// Name of the probe to run. When omitted, lists all available probe names.
    pub probe: Option<String>,
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
    /// CSS selector to match (also accepts `selector` as an alias).
    pub css: Option<String>,
    /// Alias for `css` — CSS selector to match.
    pub selector: Option<String>,
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
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

/// Parameters for the `get_diagnostics` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiagnosticsParams {
    /// Target a specific webview window by label.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_assert_params_label_is_optional() {
        // Regression: `label` was a required `String`, so a minimal
        // `{expression, condition}` call failed deserialization with an opaque
        // 400. It is now `#[serde(default)]` and must default to empty.
        let params: SemanticAssertParams = serde_json::from_value(serde_json::json!({
            "expression": "1 + 1",
            "condition": "equals",
            "expected": 2
        }))
        .expect("minimal assert_semantic call (no label) must deserialize");
        assert_eq!(params.label, "");
        assert_eq!(params.expression, "1 + 1");
        assert!(params.webview_label.is_none());
    }

    #[test]
    fn semantic_assert_params_label_still_accepted() {
        let params: SemanticAssertParams = serde_json::from_value(serde_json::json!({
            "expression": "x",
            "label": "user is logged in",
            "condition": "truthy"
        }))
        .expect("explicit label must still deserialize");
        assert_eq!(params.label, "user is logged in");
    }
}
