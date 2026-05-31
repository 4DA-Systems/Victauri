//! DOM snapshot and window state types for webview introspection.

use serde::{Deserialize, Serialize};

/// Current state of a Tauri window including geometry, visibility, and loaded URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

impl DomSnapshot {
    /// Renders the snapshot as indented accessible text (roles, names, and ref handles).
    ///
    /// # Examples
    ///
    /// ```
    /// use victauri_core::{DomSnapshot, DomElement};
    /// use std::collections::BTreeMap;
    ///
    /// let snapshot = DomSnapshot {
    ///     webview_label: "main".to_string(),
    ///     elements: vec![DomElement {
    ///         ref_id: "e1".to_string(),
    ///         tag: "button".to_string(),
    ///         role: Some("button".to_string()),
    ///         name: Some("Submit".to_string()),
    ///         text: None,
    ///         value: None,
    ///         enabled: true,
    ///         visible: true,
    ///         focusable: true,
    ///         bounds: None,
    ///         children: vec![],
    ///         attributes: BTreeMap::new(),
    ///     }],
    ///     ref_map: BTreeMap::new(),
    /// };
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
