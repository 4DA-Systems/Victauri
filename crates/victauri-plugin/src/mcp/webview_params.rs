use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Parameters for the `eval_js` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EvalJsParams {
    /// JavaScript code to evaluate in the webview. Async expressions supported.
    pub code: String,
    /// Target webview label. If omitted, targets the first available webview.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
}

/// Output format for DOM snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotFormat {
    /// Compact accessible text — 70-80% fewer tokens than JSON.
    Compact,
    /// Full JSON tree with all element attributes.
    Json,
}

impl fmt::Display for SnapshotFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compact => f.write_str("compact"),
            Self::Json => f.write_str("json"),
        }
    }
}

/// Parameters for the `snapshot` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SnapshotParams {
    /// Target webview label. If omitted, targets the first available webview.
    #[serde(alias = "window", alias = "window_label")]
    pub webview_label: Option<String>,
    /// Snapshot format: "compact" (default, accessible text) or "json" (full tree). Compact uses 70-80% fewer tokens.
    pub format: Option<SnapshotFormat>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Live-4DA dogfood (2026-06-14): agents passed the intuitive `window` (and the window-tool's
    // `window_label`) to eval_js/snapshot, whose field is `webview_label`. Serde silently dropped the
    // unknown field → None → the call hit the MAIN window instead of the targeted one. The aliases make
    // `window`, `window_label`, and `webview_label` all populate the target — so the param is never ignored.
    #[test]
    fn eval_js_accepts_window_aliases() {
        for key in ["webview_label", "window_label", "window"] {
            let json = format!(r#"{{"code":"1+1","{key}":"briefing"}}"#);
            let p: EvalJsParams = serde_json::from_str(&json).unwrap();
            assert_eq!(
                p.webview_label.as_deref(),
                Some("briefing"),
                "key `{key}` must target the window"
            );
        }
        // Omitted → None (defaults to main/first), unchanged.
        let p: EvalJsParams = serde_json::from_str(r#"{"code":"1"}"#).unwrap();
        assert_eq!(p.webview_label, None);
    }

    #[test]
    fn snapshot_accepts_window_aliases() {
        for key in ["webview_label", "window_label", "window"] {
            let json = format!(r#"{{"{key}":"notification"}}"#);
            let p: SnapshotParams = serde_json::from_str(&json).unwrap();
            assert_eq!(
                p.webview_label.as_deref(),
                Some("notification"),
                "key `{key}` must target the window"
            );
        }
    }
}
