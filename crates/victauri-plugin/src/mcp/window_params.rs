use schemars::JsonSchema;
use serde::Deserialize;

/// Parameters for the `screenshot` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScreenshotParams {
    /// Target window label. If omitted, captures the main/first visible window.
    #[serde(alias = "window", alias = "webview_label")]
    pub window_label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Live-4DA dogfood (2026-06-14): `screenshot` always captured the MAIN window because the intuitive
    // `window` (and eval_js's `webview_label`) were dropped — its field is `window_label`. Aliases fix it.
    #[test]
    fn screenshot_accepts_window_aliases() {
        for key in ["window_label", "window", "webview_label"] {
            let json = format!(r#"{{"{key}":"briefing"}}"#);
            let p: ScreenshotParams = serde_json::from_str(&json).unwrap();
            assert_eq!(
                p.window_label.as_deref(),
                Some("briefing"),
                "key `{key}` must target the window"
            );
        }
    }
}
