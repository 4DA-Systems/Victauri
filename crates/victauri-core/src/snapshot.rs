//! DOM snapshot and window state types for webview introspection.

use serde::{Deserialize, Serialize};

/// Current state of a Tauri window including geometry, visibility, and loaded URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WindowState {
    /// Tauri window label (e.g. "main", "notification").
    pub label: String,
    /// Window title bar text.
    pub title: String,
    /// URL currently loaded in the webview.
    pub url: String,
    /// Whether the window is visible on screen.
    pub visible: bool,
    /// Whether the window currently has input focus.
    pub focused: bool,
    /// Whether the window is maximized.
    pub maximized: bool,
    /// Whether the window is minimized.
    pub minimized: bool,
    /// Whether the window is in fullscreen mode.
    pub fullscreen: bool,
    /// Window position as (x, y) in screen coordinates.
    pub position: (i32, i32),
    /// Window dimensions as (width, height) in pixels.
    pub size: (u32, u32),
}

/// A point-in-time snapshot of the DOM accessible tree from a specific webview.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DomSnapshot {
    /// Label of the webview this snapshot was taken from.
    pub webview_label: String,
    /// Top-level accessible elements in the DOM tree.
    pub elements: Vec<DomElement>,
    /// Maps ref IDs to CSS selectors for element lookup.
    pub ref_map: std::collections::BTreeMap<String, String>,
}

/// A single element in the accessible DOM tree with semantic metadata and ref handle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DomElement {
    /// Unique ref handle for this element (e.g. "e3"), used to target interactions.
    pub ref_id: String,
    /// HTML tag name (e.g. "div", "button").
    pub tag: String,
    /// ARIA role if present (e.g. "button", "navigation").
    pub role: Option<String>,
    /// Accessible name derived from aria-label, text content, or other heuristics.
    pub name: Option<String>,
    /// Visible text content of the element.
    pub text: Option<String>,
    /// Form input value, if applicable.
    pub value: Option<String>,
    /// Whether the element is interactive (not disabled).
    pub enabled: bool,
    /// Whether the element is visible in the viewport.
    pub visible: bool,
    /// Whether the element can receive keyboard focus.
    pub focusable: bool,
    /// Pixel-level bounding rectangle, if available.
    pub bounds: Option<ElementBounds>,
    /// Nested child elements forming the accessible subtree.
    pub children: Vec<Self>,
    /// Raw HTML attributes on the element.
    pub attributes: std::collections::BTreeMap<String, String>,
}

/// Pixel-level bounding rectangle of a DOM element relative to the viewport.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ElementBounds {
    /// Left edge offset from viewport origin.
    pub x: f64,
    /// Top edge offset from viewport origin.
    pub y: f64,
    /// Element width in CSS pixels.
    pub width: f64,
    /// Element height in CSS pixels.
    pub height: f64,
}

impl WindowState {
    /// Creates a window state for `label` with every other field zeroed
    /// (empty title/URL, not visible/focused/maximized/minimized/fullscreen,
    /// position `(0, 0)`, size `(0, 0)`). Chain the `with_*` setters to fill it in.
    ///
    /// `WindowState` is `#[non_exhaustive]`; `victauri_plugin::bridge::WebviewBridge`
    /// implementors and mock bridges outside this crate build it through this constructor.
    ///
    /// # Examples
    ///
    /// ```
    /// use victauri_core::WindowState;
    ///
    /// let w = WindowState::new("main")
    ///     .with_title("My App")
    ///     .with_url("http://localhost/")
    ///     .with_visible(true)
    ///     .with_size(800, 600);
    /// assert_eq!(w.label, "main");
    /// assert_eq!(w.size, (800, 600));
    /// assert!(!w.focused);
    /// ```
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            title: String::new(),
            url: String::new(),
            visible: false,
            focused: false,
            maximized: false,
            minimized: false,
            fullscreen: false,
            position: (0, 0),
            size: (0, 0),
        }
    }

    /// Sets the window title.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Sets the URL loaded in the webview.
    #[must_use]
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// Sets whether the window is visible.
    #[must_use]
    pub fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Sets whether the window has input focus.
    #[must_use]
    pub fn with_focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Sets whether the window is maximized.
    #[must_use]
    pub fn with_maximized(mut self, maximized: bool) -> Self {
        self.maximized = maximized;
        self
    }

    /// Sets whether the window is minimized.
    #[must_use]
    pub fn with_minimized(mut self, minimized: bool) -> Self {
        self.minimized = minimized;
        self
    }

    /// Sets whether the window is fullscreen.
    #[must_use]
    pub fn with_fullscreen(mut self, fullscreen: bool) -> Self {
        self.fullscreen = fullscreen;
        self
    }

    /// Sets the window position in screen coordinates.
    #[must_use]
    pub fn with_position(mut self, x: i32, y: i32) -> Self {
        self.position = (x, y);
        self
    }

    /// Sets the window size in pixels.
    #[must_use]
    pub fn with_size(mut self, width: u32, height: u32) -> Self {
        self.size = (width, height);
        self
    }
}

impl DomSnapshot {
    /// Renders the snapshot as indented accessible text (roles, names, and ref handles).
    ///
    /// # Examples
    ///
    /// ```
    /// use victauri_core::DomSnapshot;
    ///
    /// // Snapshots are produced by the webview bridge; here one is parsed from JSON.
    /// let snapshot: DomSnapshot = serde_json::from_value(serde_json::json!({
    ///     "webview_label": "main",
    ///     "elements": [{
    ///         "ref_id": "e1", "tag": "button", "role": "button", "name": "Submit",
    ///         "text": null, "value": null, "enabled": true, "visible": true,
    ///         "focusable": true, "bounds": null, "children": [], "attributes": {}
    ///     }],
    ///     "ref_map": {}
    /// }))
    /// .unwrap();
    /// let text = snapshot.to_accessible_text(0);
    /// assert!(text.contains("button"));
    /// assert!(text.contains("Submit"));
    /// assert!(text.contains("[ref=e1]"));
    /// ```
    #[must_use]
    pub fn to_accessible_text(&self, indent: usize) -> String {
        let mut output = String::new();
        for element in &self.elements {
            Self::format_element(&mut output, element, indent, 0);
        }
        output
    }

    /// Maximum DOM nesting `format_element` will recurse through. A snapshot can
    /// come from an arbitrary (browser-extension: hostile) page, so an unbounded
    /// recursion here is a stack-overflow (denial of service) when rendering the text.
    const MAX_DOM_DEPTH: usize = 256;

    fn format_element(output: &mut String, element: &DomElement, indent: usize, depth: usize) {
        if !element.visible {
            return;
        }

        if depth >= Self::MAX_DOM_DEPTH {
            output.push_str(&format!(
                "{}- <max DOM depth {} exceeded>\n",
                "  ".repeat(indent),
                Self::MAX_DOM_DEPTH
            ));
            return;
        }

        let prefix = "  ".repeat(indent);

        let role_str = element.role.as_deref().unwrap_or(&element.tag);
        let name_str = element
            .name
            .as_ref()
            .map(|n| format!(" \"{n}\""))
            .unwrap_or_default();
        let ref_str = if element.focusable || element.tag == "button" || element.tag == "input" {
            format!(" [ref={}]", element.ref_id)
        } else {
            String::new()
        };

        let line = format!("{prefix}- {role_str}{name_str}{ref_str}\n");
        output.push_str(&line);

        for child in &element.children {
            Self::format_element(output, child, indent + 1, depth + 1);
        }
    }
}
