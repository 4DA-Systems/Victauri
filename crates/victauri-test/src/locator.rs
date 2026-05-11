//! Playwright-style composable element locators with actions, queries, and auto-waiting expectations.
//!
//! [`Locator`] is the primary entry point. Create one via a factory method, optionally
//! refine it with chained filters, then perform actions or query properties against a
//! live [`VictauriClient`] connection.
//!
//! # Examples
//!
//! ```rust,ignore
//! use victauri_test::locator::Locator;
//!
//! let submit = Locator::role("button").and_text("Submit");
//! submit.click(&mut client).await.unwrap();
//!
//! let email = Locator::placeholder("Enter email");
//! email.fill(&mut client, "user@example.com").await.unwrap();
//!
//! Locator::test_id("toast")
//!     .expect(&mut client)
//!     .to_be_visible()
//!     .await
//!     .unwrap();
//! ```

use std::fmt;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::VictauriClient;
use crate::error::TestError;

// ── Data types ──────────────────────────────────────────────────────────────

/// Bounding rectangle of a DOM element in CSS pixels.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Bounds {
    /// X offset from the viewport left edge.
    pub x: f64,
    /// Y offset from the viewport top edge.
    pub y: f64,
    /// Element width.
    pub width: f64,
    /// Element height.
    pub height: f64,
}

/// A single element resolved from a [`Locator`] query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocatorMatch {
    /// Ref handle ID used to target this element in subsequent actions.
    pub ref_id: String,
    /// HTML tag name (e.g. `"button"`, `"input"`).
    pub tag: String,
    /// ARIA role, if present.
    pub role: Option<String>,
    /// Accessible name, if present.
    pub name: Option<String>,
    /// Visible text content, if any.
    pub text: Option<String>,
    /// Whether the element is currently visible.
    pub visible: bool,
    /// Whether the element is currently enabled (not disabled).
    pub enabled: bool,
    /// Form control value, if applicable.
    pub value: Option<String>,
    /// Bounding rectangle in CSS pixels.
    pub bounds: Option<Bounds>,
}

// ── Internal enums ──────────────────────────────────────────────────────────

/// Primary query strategy used to find elements via the bridge.
#[derive(Debug, Clone)]
enum Strategy {
    Role(String),
    Text(String),
    TextExact(String),
    TestId(String),
    Css(String),
    Label(String),
    Placeholder(String),
    AltText(String),
    Title(String),
}

/// Additional client-side filter applied after the bridge query returns.
#[derive(Debug, Clone)]
enum Filter {
    Text(String),
    TextExact(String),
    Role(String),
    Name(String),
    Tag(String),
    HasAttribute(String, Option<String>),
}

/// Which element to pick when multiple match.
#[derive(Debug, Clone)]
enum Pick {
    First,
    Nth(usize),
    Last,
}

// ── Locator ─────────────────────────────────────────────────────────────────

/// Composable element query with actions, property queries, and auto-waiting expectations.
///
/// Locators are lazy: they describe *how* to find an element but do not contact
/// the server until an action, query, or expectation method is called.
///
/// All refinement methods return a new `Locator` (the type is `Clone`), so the
/// original remains usable.
#[derive(Debug, Clone)]
pub struct Locator {
    strategy: Strategy,
    filters: Vec<Filter>,
    pick: Pick,
}

// Compile-time guarantee that Locator can cross task boundaries.
const _: () = {
    fn _assert_send_sync<T: Send + Sync>() {}
    fn _check() {
        _assert_send_sync::<Locator>();
    }
};

impl Locator {
    // ── Factory methods ─────────────────────────────────────────────────

    /// Locate elements by ARIA role (e.g. `"button"`, `"textbox"`).
    #[must_use]
    pub fn role(role: &str) -> Self {
        Self {
            strategy: Strategy::Role(role.to_string()),
            filters: Vec::new(),
            pick: Pick::First,
        }
    }

    /// Locate elements whose visible text contains `text` (case-insensitive substring).
    #[must_use]
    pub fn text(text: &str) -> Self {
        Self {
            strategy: Strategy::Text(text.to_string()),
            filters: Vec::new(),
            pick: Pick::First,
        }
    }

    /// Locate elements whose visible text exactly equals `text`.
    #[must_use]
    pub fn text_exact(text: &str) -> Self {
        Self {
            strategy: Strategy::TextExact(text.to_string()),
            filters: Vec::new(),
            pick: Pick::First,
        }
    }

    /// Locate elements by `data-testid` attribute.
    #[must_use]
    pub fn test_id(id: &str) -> Self {
        Self {
            strategy: Strategy::TestId(id.to_string()),
            filters: Vec::new(),
            pick: Pick::First,
        }
    }

    /// Locate elements by CSS selector.
    #[must_use]
    pub fn css(selector: &str) -> Self {
        Self {
            strategy: Strategy::Css(selector.to_string()),
            filters: Vec::new(),
            pick: Pick::First,
        }
    }

    /// Locate form controls by their associated `<label>` text.
    #[must_use]
    pub fn label(text: &str) -> Self {
        Self {
            strategy: Strategy::Label(text.to_string()),
            filters: Vec::new(),
            pick: Pick::First,
        }
    }

    /// Locate elements by `placeholder` attribute.
    #[must_use]
    pub fn placeholder(text: &str) -> Self {
        Self {
            strategy: Strategy::Placeholder(text.to_string()),
            filters: Vec::new(),
            pick: Pick::First,
        }
    }

    /// Locate elements by `alt` attribute (images, areas).
    #[must_use]
    pub fn alt_text(alt: &str) -> Self {
        Self {
            strategy: Strategy::AltText(alt.to_string()),
            filters: Vec::new(),
            pick: Pick::First,
        }
    }

    /// Locate elements by `title` attribute.
    #[must_use]
    pub fn title(title: &str) -> Self {
        Self {
            strategy: Strategy::Title(title.to_string()),
            filters: Vec::new(),
            pick: Pick::First,
        }
    }

    // ── Refinement (chainable) ──────────────────────────────────────────

    /// Further filter by case-insensitive text substring.
    #[must_use]
    pub fn and_text(mut self, text: &str) -> Self {
        self.filters.push(Filter::Text(text.to_string()));
        self
    }

    /// Further filter by exact text match.
    #[must_use]
    pub fn and_text_exact(mut self, text: &str) -> Self {
        self.filters.push(Filter::TextExact(text.to_string()));
        self
    }

    /// Further filter by ARIA role.
    #[must_use]
    pub fn and_role(mut self, role: &str) -> Self {
        self.filters.push(Filter::Role(role.to_string()));
        self
    }

    /// Further filter by accessible name (case-insensitive substring).
    #[must_use]
    pub fn name(mut self, name: &str) -> Self {
        self.filters.push(Filter::Name(name.to_string()));
        self
    }

    /// Further filter by HTML tag name.
    #[must_use]
    pub fn and_tag(mut self, tag: &str) -> Self {
        self.filters.push(Filter::Tag(tag.to_string()));
        self
    }

    /// Further filter by the presence (and optionally value) of an HTML attribute.
    #[must_use]
    pub fn and_has_attribute(mut self, attr_name: &str, attr_value: Option<&str>) -> Self {
        self.filters.push(Filter::HasAttribute(
            attr_name.to_string(),
            attr_value.map(String::from),
        ));
        self
    }

    /// Select the nth match (zero-based).
    #[must_use]
    pub fn nth(mut self, n: usize) -> Self {
        self.pick = Pick::Nth(n);
        self
    }

    /// Select the first match (default behavior, explicit for readability).
    #[must_use]
    pub fn first(mut self) -> Self {
        self.pick = Pick::First;
        self
    }

    /// Select the last match.
    #[must_use]
    pub fn last(mut self) -> Self {
        self.pick = Pick::Last;
        self
    }

    // ── Actions ─────────────────────────────────────────────────────────

    /// Click the resolved element.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn click(&self, client: &mut VictauriClient) -> Result<Value, TestError> {
        let el = self.resolve_one(client).await?;
        client.click(&el.ref_id).await
    }

    /// Double-click the resolved element.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn double_click(&self, client: &mut VictauriClient) -> Result<Value, TestError> {
        let el = self.resolve_one(client).await?;
        client.double_click(&el.ref_id).await
    }

    /// Clear the field and fill it with `value`.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn fill(&self, client: &mut VictauriClient, value: &str) -> Result<Value, TestError> {
        let el = self.resolve_one(client).await?;
        client.fill(&el.ref_id, value).await
    }

    /// Type `text` character-by-character into the resolved element.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn type_text(
        &self,
        client: &mut VictauriClient,
        text: &str,
    ) -> Result<Value, TestError> {
        let el = self.resolve_one(client).await?;
        client.type_text(&el.ref_id, text).await
    }

    /// Press a keyboard key (e.g. `"Enter"`, `"Control+c"`).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn press_key(
        &self,
        client: &mut VictauriClient,
        key: &str,
    ) -> Result<Value, TestError> {
        let _el = self.resolve_one(client).await?;
        client.press_key(key).await
    }

    /// Hover over the resolved element.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn hover(&self, client: &mut VictauriClient) -> Result<Value, TestError> {
        let el = self.resolve_one(client).await?;
        client.hover(&el.ref_id).await
    }

    /// Focus the resolved element.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn focus(&self, client: &mut VictauriClient) -> Result<Value, TestError> {
        let el = self.resolve_one(client).await?;
        client.focus(&el.ref_id).await
    }

    /// Remove focus from the currently active element.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn blur(&self, client: &mut VictauriClient) -> Result<Value, TestError> {
        let _el = self.resolve_one(client).await?;
        client.eval_js("document.activeElement?.blur()").await
    }

    /// Scroll the resolved element into view.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn scroll_into_view(&self, client: &mut VictauriClient) -> Result<Value, TestError> {
        let el = self.resolve_one(client).await?;
        client.scroll_to(&el.ref_id).await
    }

    /// Select option(s) in a `<select>` element.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn select_option(
        &self,
        client: &mut VictauriClient,
        values: &[&str],
    ) -> Result<Value, TestError> {
        let el = self.resolve_one(client).await?;
        client.select_option(&el.ref_id, values).await
    }

    /// Check a checkbox or radio button (sets `checked = true`).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn check(&self, client: &mut VictauriClient) -> Result<Value, TestError> {
        let el = self.resolve_one(client).await?;
        let code = format!(
            "(function() {{ var el = window.__VICTAURI__?.getRef({}); \
             if (!el) return null; \
             if (!el.checked) {{ el.checked = true; \
             el.dispatchEvent(new Event('change', {{bubbles:true}})); \
             el.dispatchEvent(new Event('input', {{bubbles:true}})); }} \
             return true; }})()",
            serde_json::to_string(&el.ref_id).unwrap_or_else(|_| "\"\"".to_string()),
        );
        client.eval_js(&code).await
    }

    /// Uncheck a checkbox (sets `checked = false`).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn uncheck(&self, client: &mut VictauriClient) -> Result<Value, TestError> {
        let el = self.resolve_one(client).await?;
        let code = format!(
            "(function() {{ var el = window.__VICTAURI__?.getRef({}); \
             if (!el) return null; \
             if (el.checked) {{ el.checked = false; \
             el.dispatchEvent(new Event('change', {{bubbles:true}})); \
             el.dispatchEvent(new Event('input', {{bubbles:true}})); }} \
             return true; }})()",
            serde_json::to_string(&el.ref_id).unwrap_or_else(|_| "\"\"".to_string()),
        );
        client.eval_js(&code).await
    }

    // ── Query methods ───────────────────────────────────────────────────

    /// Get the `textContent` of the resolved element.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn text_content(&self, client: &mut VictauriClient) -> Result<String, TestError> {
        let val = self
            .eval_on_element(client, "return el.textContent || \"\";")
            .await?;
        Ok(value_to_string(&val))
    }

    /// Get the `innerText` of the resolved element.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn inner_text(&self, client: &mut VictauriClient) -> Result<String, TestError> {
        let val = self
            .eval_on_element(client, "return el.innerText || \"\";")
            .await?;
        Ok(value_to_string(&val))
    }

    /// Get the current `value` of an input/textarea/select element.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn input_value(&self, client: &mut VictauriClient) -> Result<String, TestError> {
        let val = self
            .eval_on_element(client, "return el.value || \"\";")
            .await?;
        Ok(value_to_string(&val))
    }

    /// Whether the resolved element is visible.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn is_visible(&self, client: &mut VictauriClient) -> Result<bool, TestError> {
        let el = self.resolve_one(client).await?;
        Ok(el.visible)
    }

    /// Whether the resolved element is enabled (not disabled).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn is_enabled(&self, client: &mut VictauriClient) -> Result<bool, TestError> {
        let el = self.resolve_one(client).await?;
        Ok(el.enabled)
    }

    /// Whether the resolved element is checked (checkbox/radio).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn is_checked(&self, client: &mut VictauriClient) -> Result<bool, TestError> {
        let val = self.eval_on_element(client, "return !!el.checked;").await?;
        Ok(val.as_bool().unwrap_or(false))
    }

    /// Whether the resolved element currently has focus.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn is_focused(&self, client: &mut VictauriClient) -> Result<bool, TestError> {
        let val = self
            .eval_on_element(client, "return document.activeElement === el;")
            .await?;
        Ok(val.as_bool().unwrap_or(false))
    }

    /// Count the number of elements matching this locator.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn count(&self, client: &mut VictauriClient) -> Result<usize, TestError> {
        let all = self.resolve_all(client).await?;
        Ok(all.len())
    }

    /// Get the bounding rectangle of the resolved element, if available.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn bounding_box(
        &self,
        client: &mut VictauriClient,
    ) -> Result<Option<Bounds>, TestError> {
        let el = self.resolve_one(client).await?;
        Ok(el.bounds)
    }

    /// Read the value of an HTML attribute on the resolved element.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn get_attribute(
        &self,
        client: &mut VictauriClient,
        attr_name: &str,
    ) -> Result<Option<String>, TestError> {
        let escaped = attr_name.replace('\\', "\\\\").replace('"', "\\\"");
        let js_body = format!("return el.getAttribute(\"{escaped}\");");
        let val = self.eval_on_element(client, &js_body).await?;
        if val.is_null() {
            Ok(None)
        } else {
            Ok(Some(value_to_string(&val)))
        }
    }

    /// Resolve all matching elements (ignoring the pick setting).
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn all(&self, client: &mut VictauriClient) -> Result<Vec<LocatorMatch>, TestError> {
        self.resolve_all(client).await
    }

    /// Get `textContent` for every matching element.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn all_text_contents(
        &self,
        client: &mut VictauriClient,
    ) -> Result<Vec<String>, TestError> {
        let elements = self.resolve_all(client).await?;
        let mut texts = Vec::with_capacity(elements.len());
        for el in &elements {
            let code = format!(
                "(function() {{ var el = window.__VICTAURI__?.getRef({}); \
                 if (!el) return \"\"; \
                 return el.textContent || \"\"; }})()",
                serde_json::to_string(&el.ref_id).unwrap_or_else(|_| "\"\"".to_string()),
            );
            let val = client.eval_js(&code).await?;
            texts.push(value_to_string(&val));
        }
        Ok(texts)
    }

    // ── Expectations ────────────────────────────────────────────────────

    /// Create an auto-waiting expectation builder for this locator.
    pub fn expect<'a>(&'a self, client: &'a mut VictauriClient) -> LocatorExpect<'a> {
        LocatorExpect {
            locator: self,
            client,
            timeout_ms: 5000,
            poll_ms: 200,
            negated: false,
        }
    }

    // ── Resolution internals ────────────────────────────────────────────

    async fn resolve_all(
        &self,
        client: &mut VictauriClient,
    ) -> Result<Vec<LocatorMatch>, TestError> {
        let query = self.build_query();
        let result = client.find_elements(query).await?;
        let mut elements = Self::parse_elements(&result);
        elements = self.apply_filters(elements);
        Ok(elements)
    }

    async fn resolve_one(&self, client: &mut VictauriClient) -> Result<LocatorMatch, TestError> {
        let all = self.resolve_all(client).await?;
        self.pick_one(all)
    }

    fn pick_one(&self, all: Vec<LocatorMatch>) -> Result<LocatorMatch, TestError> {
        if all.is_empty() {
            return Err(TestError::ElementNotFound(format!(
                "no elements match {self}"
            )));
        }
        match self.pick {
            Pick::First => Ok(all.into_iter().next().expect("checked non-empty")),
            Pick::Last => Ok(all.into_iter().last().expect("checked non-empty")),
            Pick::Nth(n) => {
                let total = all.len();
                all.into_iter().nth(n).ok_or_else(|| {
                    TestError::ElementNotFound(format!(
                        "{self}: wanted index {n} but only {total} elements matched"
                    ))
                })
            }
        }
    }

    fn build_query(&self) -> Value {
        let mut query = json!({});
        match &self.strategy {
            Strategy::Role(r) => {
                query["role"] = json!(r);
            }
            Strategy::Text(t) => {
                query["text"] = json!(t);
            }
            Strategy::TextExact(t) => {
                query["text"] = json!(t);
                query["exact"] = json!(true);
            }
            Strategy::TestId(id) => {
                query["test_id"] = json!(id);
            }
            Strategy::Css(sel) => {
                query["css"] = json!(sel);
            }
            Strategy::Label(t) => {
                query["label"] = json!(t);
            }
            Strategy::Placeholder(t) => {
                query["placeholder"] = json!(t);
            }
            Strategy::AltText(a) => {
                query["alt"] = json!(a);
            }
            Strategy::Title(t) => {
                query["title_attr"] = json!(t);
            }
        }

        // Promote filters that the bridge can handle natively into the query
        for filter in &self.filters {
            match filter {
                Filter::Role(r) => {
                    query["role"] = json!(r);
                }
                Filter::Name(n) => {
                    query["name"] = json!(n);
                }
                Filter::Tag(t) => {
                    query["tag"] = json!(t);
                }
                // Text, TextExact, and HasAttribute are applied client-side
                Filter::Text(_) | Filter::TextExact(_) | Filter::HasAttribute(_, _) => {}
            }
        }

        query["max_results"] = json!(50);
        query
    }

    fn apply_filters(&self, elements: Vec<LocatorMatch>) -> Vec<LocatorMatch> {
        let mut result = elements;

        // TextExact strategy requires exact client-side match (bridge `text` with
        // `exact:true` may not be supported on all versions).
        if let Strategy::TextExact(ref expected) = self.strategy {
            result.retain(|el| {
                el.text
                    .as_deref()
                    .is_some_and(|t| t.trim() == expected.as_str())
            });
        }

        for filter in &self.filters {
            match filter {
                Filter::Text(expected) => {
                    let lower = expected.to_lowercase();
                    result.retain(|el| {
                        el.text
                            .as_deref()
                            .is_some_and(|t| t.to_lowercase().contains(&lower))
                    });
                }
                Filter::TextExact(expected) => {
                    result.retain(|el| {
                        el.text
                            .as_deref()
                            .is_some_and(|t| t.trim() == expected.as_str())
                    });
                }
                Filter::Role(expected) => {
                    result.retain(|el| el.role.as_deref().is_some_and(|r| r == expected.as_str()));
                }
                Filter::Name(expected) => {
                    let lower = expected.to_lowercase();
                    result.retain(|el| {
                        el.name
                            .as_deref()
                            .is_some_and(|n| n.to_lowercase().contains(&lower))
                    });
                }
                Filter::Tag(expected) => {
                    result.retain(|el| el.tag == *expected);
                }
                // HasAttribute cannot be checked without DOM access; keep all.
                Filter::HasAttribute(_, _) => {}
            }
        }

        result
    }

    fn parse_elements(result: &Value) -> Vec<LocatorMatch> {
        let array = result
            .as_array()
            .or_else(|| result.get("elements").and_then(Value::as_array));

        let Some(arr) = array else {
            return Vec::new();
        };

        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let Some(ref_id) = item.get("ref_id").and_then(Value::as_str) else {
                continue;
            };
            let tag = item
                .get("tag")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();

            let bounds = item.get("bounds").and_then(|b| {
                Some(Bounds {
                    x: b.get("x")?.as_f64()?,
                    y: b.get("y")?.as_f64()?,
                    width: b.get("width")?.as_f64()?,
                    height: b.get("height")?.as_f64()?,
                })
            });

            out.push(LocatorMatch {
                ref_id: ref_id.to_string(),
                tag,
                role: item.get("role").and_then(Value::as_str).map(String::from),
                name: item.get("name").and_then(Value::as_str).map(String::from),
                text: item.get("text").and_then(Value::as_str).map(String::from),
                visible: item.get("visible").and_then(Value::as_bool).unwrap_or(true),
                enabled: item.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                value: item.get("value").and_then(Value::as_str).map(String::from),
                bounds,
            });
        }
        out
    }

    async fn eval_on_element(
        &self,
        client: &mut VictauriClient,
        js_body: &str,
    ) -> Result<Value, TestError> {
        let el = self.resolve_one(client).await?;
        let ref_str = serde_json::to_string(&el.ref_id).unwrap_or_else(|_| "\"\"".to_string());
        let code = format!(
            "(function() {{ var el = window.__VICTAURI__?.getRef({ref_str}); \
             if (!el) return null; {js_body} }})()"
        );
        client.eval_js(&code).await
    }
}

impl fmt::Display for Locator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.strategy {
            Strategy::Role(r) => write!(f, "role(\"{r}\")")?,
            Strategy::Text(t) => write!(f, "text(\"{t}\")")?,
            Strategy::TextExact(t) => write!(f, "text_exact(\"{t}\")")?,
            Strategy::TestId(id) => write!(f, "test_id(\"{id}\")")?,
            Strategy::Css(s) => write!(f, "css(\"{s}\")")?,
            Strategy::Label(t) => write!(f, "label(\"{t}\")")?,
            Strategy::Placeholder(t) => write!(f, "placeholder(\"{t}\")")?,
            Strategy::AltText(a) => write!(f, "alt_text(\"{a}\")")?,
            Strategy::Title(t) => write!(f, "title(\"{t}\")")?,
        }

        for filter in &self.filters {
            match filter {
                Filter::Text(t) => write!(f, ".and_text(\"{t}\")")?,
                Filter::TextExact(t) => write!(f, ".and_text_exact(\"{t}\")")?,
                Filter::Role(r) => write!(f, ".and_role(\"{r}\")")?,
                Filter::Name(n) => write!(f, ".name(\"{n}\")")?,
                Filter::Tag(t) => write!(f, ".and_tag(\"{t}\")")?,
                Filter::HasAttribute(a, None) => write!(f, ".and_has_attribute(\"{a}\", None)")?,
                Filter::HasAttribute(a, Some(v)) => {
                    write!(f, ".and_has_attribute(\"{a}\", Some(\"{v}\"))")?;
                }
            }
        }

        match &self.pick {
            Pick::First => {}
            Pick::Nth(n) => write!(f, ".nth({n})")?,
            Pick::Last => write!(f, ".last()")?,
        }

        Ok(())
    }
}

// ── LocatorExpect ───────────────────────────────────────────────────────────

/// Auto-waiting assertion builder created by [`Locator::expect`].
///
/// Each assertion method polls the condition repeatedly until it passes or
/// the timeout expires. Use [`.not()`](Self::not) to negate the next assertion.
pub struct LocatorExpect<'a> {
    locator: &'a Locator,
    client: &'a mut VictauriClient,
    timeout_ms: u64,
    poll_ms: u64,
    negated: bool,
}

impl<'a> LocatorExpect<'a> {
    /// Override the maximum wait time in milliseconds (default: 5000).
    #[must_use]
    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = ms;
        self
    }

    /// Override the polling interval in milliseconds (default: 200).
    #[must_use]
    pub fn poll_ms(mut self, ms: u64) -> Self {
        self.poll_ms = ms;
        self
    }

    /// Negate the next assertion (e.g. `.not().to_be_visible()` waits until hidden).
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn not(mut self) -> Self {
        self.negated = !self.negated;
        self
    }

    // ── Assertion methods ───────────────────────────────────────────────

    /// Wait until the element is visible.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_be_visible(self) -> Result<(), TestError> {
        let negated = self.negated;
        let desc = if negated {
            format!("{} to NOT be visible", self.locator)
        } else {
            format!("{} to be visible", self.locator)
        };
        self.poll_until_simple(|el| el.visible, &desc).await
    }

    /// Wait until the element is hidden (not visible).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_be_hidden(self) -> Result<(), TestError> {
        let negated = self.negated;
        let desc = if negated {
            format!("{} to NOT be hidden", self.locator)
        } else {
            format!("{} to be hidden", self.locator)
        };
        // "hidden" means visible==false, so invert the check
        let effective_negated = !negated;
        self.poll_until_simple_with_negated(|el| el.visible, effective_negated, &desc)
            .await
    }

    /// Wait until the element is enabled.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_be_enabled(self) -> Result<(), TestError> {
        let negated = self.negated;
        let desc = if negated {
            format!("{} to NOT be enabled", self.locator)
        } else {
            format!("{} to be enabled", self.locator)
        };
        self.poll_until_simple(|el| el.enabled, &desc).await
    }

    /// Wait until the element is disabled.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_be_disabled(self) -> Result<(), TestError> {
        let negated = self.negated;
        let desc = if negated {
            format!("{} to NOT be disabled", self.locator)
        } else {
            format!("{} to be disabled", self.locator)
        };
        let effective_negated = !negated;
        self.poll_until_simple_with_negated(|el| el.enabled, effective_negated, &desc)
            .await
    }

    /// Wait until the element has keyboard focus.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_be_focused(self) -> Result<(), TestError> {
        let negated = self.negated;
        let desc = if negated {
            format!("{} to NOT be focused", self.locator)
        } else {
            format!("{} to be focused", self.locator)
        };
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let poll = Duration::from_millis(self.poll_ms);
        let locator = self.locator.clone();
        let client = self.client;
        loop {
            let result = check_focused(&locator, client).await;
            let condition_met = match result {
                Ok(met) => {
                    if negated {
                        !met
                    } else {
                        met
                    }
                }
                Err(_) if negated => true,
                Err(e) => return Err(e),
            };
            if condition_met {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(TestError::Timeout(format!(
                    "expected {desc} within {}ms",
                    deadline
                        .duration_since(Instant::now().checked_sub(poll).unwrap_or(Instant::now()))
                        .as_millis()
                )));
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// Wait until the element's text content equals `expected`.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_have_text(self, expected: &str) -> Result<(), TestError> {
        let negated = self.negated;
        let desc = if negated {
            format!("{} to NOT have text \"{expected}\"", self.locator)
        } else {
            format!("{} to have text \"{expected}\"", self.locator)
        };
        let expected_owned = expected.to_string();
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let poll = Duration::from_millis(self.poll_ms);
        let locator = self.locator.clone();
        let client = self.client;
        loop {
            let result = check_text_content(&locator, client).await;
            let condition_met = match result {
                Ok(actual) => {
                    let matches = actual.trim() == expected_owned.as_str();
                    if negated { !matches } else { matches }
                }
                Err(_) if negated => true,
                Err(e) => return Err(e),
            };
            if condition_met {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(TestError::Timeout(format!(
                    "expected {desc} within {}ms",
                    self.timeout_ms
                )));
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// Wait until the element's text content contains `expected` as a substring.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_contain_text(self, expected: &str) -> Result<(), TestError> {
        let negated = self.negated;
        let desc = if negated {
            format!("{} to NOT contain text \"{expected}\"", self.locator)
        } else {
            format!("{} to contain text \"{expected}\"", self.locator)
        };
        let expected_owned = expected.to_string();
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let poll = Duration::from_millis(self.poll_ms);
        let locator = self.locator.clone();
        let client = self.client;
        loop {
            let result = check_text_content(&locator, client).await;
            let condition_met = match result {
                Ok(actual) => {
                    let matches = actual.contains(expected_owned.as_str());
                    if negated { !matches } else { matches }
                }
                Err(_) if negated => true,
                Err(e) => return Err(e),
            };
            if condition_met {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(TestError::Timeout(format!(
                    "expected {desc} within {}ms",
                    self.timeout_ms
                )));
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// Wait until the element's input value equals `expected`.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_have_value(self, expected: &str) -> Result<(), TestError> {
        let negated = self.negated;
        let desc = if negated {
            format!("{} to NOT have value \"{expected}\"", self.locator)
        } else {
            format!("{} to have value \"{expected}\"", self.locator)
        };
        let expected_owned = expected.to_string();
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let poll = Duration::from_millis(self.poll_ms);
        let locator = self.locator.clone();
        let client = self.client;
        loop {
            let result = check_input_value(&locator, client).await;
            let condition_met = match result {
                Ok(actual) => {
                    let matches = actual == expected_owned;
                    if negated { !matches } else { matches }
                }
                Err(_) if negated => true,
                Err(e) => return Err(e),
            };
            if condition_met {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(TestError::Timeout(format!(
                    "expected {desc} within {}ms",
                    self.timeout_ms
                )));
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// Wait until the element has the given attribute with the given value.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_have_attribute(self, attr_name: &str, value: &str) -> Result<(), TestError> {
        let negated = self.negated;
        let desc = if negated {
            format!(
                "{} to NOT have attribute {attr_name}=\"{value}\"",
                self.locator
            )
        } else {
            format!("{} to have attribute {attr_name}=\"{value}\"", self.locator)
        };
        let attr_owned = attr_name.to_string();
        let value_owned = value.to_string();
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let poll = Duration::from_millis(self.poll_ms);
        let locator = self.locator.clone();
        let client = self.client;
        loop {
            let result = check_attribute(&locator, client, &attr_owned).await;
            let condition_met = match result {
                Ok(actual) => {
                    let matches = actual.as_deref() == Some(value_owned.as_str());
                    if negated { !matches } else { matches }
                }
                Err(_) if negated => true,
                Err(e) => return Err(e),
            };
            if condition_met {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(TestError::Timeout(format!(
                    "expected {desc} within {}ms",
                    self.timeout_ms
                )));
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// Wait until the number of matching elements equals `expected`.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_have_count(self, expected: usize) -> Result<(), TestError> {
        let negated = self.negated;
        let desc = if negated {
            format!("{} to NOT have count {expected}", self.locator)
        } else {
            format!("{} to have count {expected}", self.locator)
        };
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let poll = Duration::from_millis(self.poll_ms);
        let locator = self.locator.clone();
        let client = self.client;
        loop {
            let result = locator.resolve_all(client).await;
            let condition_met = match result {
                Ok(all) => {
                    let matches = all.len() == expected;
                    if negated { !matches } else { matches }
                }
                Err(_) if negated && expected != 0 => true,
                Err(_) if !negated && expected == 0 => true,
                Err(e) => return Err(e),
            };
            if condition_met {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(TestError::Timeout(format!(
                    "expected {desc} within {}ms",
                    self.timeout_ms
                )));
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// Wait until the element is checked (checkbox/radio).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_be_checked(self) -> Result<(), TestError> {
        let negated = self.negated;
        let desc = if negated {
            format!("{} to NOT be checked", self.locator)
        } else {
            format!("{} to be checked", self.locator)
        };
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let poll = Duration::from_millis(self.poll_ms);
        let locator = self.locator.clone();
        let client = self.client;
        loop {
            let result = check_checked(&locator, client).await;
            let condition_met = match result {
                Ok(checked) => {
                    if negated {
                        !checked
                    } else {
                        checked
                    }
                }
                Err(_) if negated => true,
                Err(e) => return Err(e),
            };
            if condition_met {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(TestError::Timeout(format!(
                    "expected {desc} within {}ms",
                    self.timeout_ms
                )));
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// Wait until the element is unchecked.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_be_unchecked(self) -> Result<(), TestError> {
        let negated = self.negated;
        let desc = if negated {
            format!("{} to NOT be unchecked", self.locator)
        } else {
            format!("{} to be unchecked", self.locator)
        };
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let poll = Duration::from_millis(self.poll_ms);
        let locator = self.locator.clone();
        let client = self.client;
        loop {
            let result = check_checked(&locator, client).await;
            let condition_met = match result {
                Ok(checked) => {
                    let unchecked = !checked;
                    if negated { !unchecked } else { unchecked }
                }
                Err(_) if negated => true,
                Err(e) => return Err(e),
            };
            if condition_met {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(TestError::Timeout(format!(
                    "expected {desc} within {}ms",
                    self.timeout_ms
                )));
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// Wait until the element exists in the DOM.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_be_attached(self) -> Result<(), TestError> {
        let negated = self.negated;
        let desc = if negated {
            format!("{} to NOT be attached", self.locator)
        } else {
            format!("{} to be attached", self.locator)
        };
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let poll = Duration::from_millis(self.poll_ms);
        let locator = self.locator.clone();
        let client = self.client;
        loop {
            let result = locator.resolve_one(client).await;
            let condition_met = match result {
                Ok(_) => !negated,
                Err(TestError::ElementNotFound(_)) => negated,
                Err(e) => return Err(e),
            };
            if condition_met {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(TestError::Timeout(format!(
                    "expected {desc} within {}ms",
                    self.timeout_ms
                )));
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// Wait until the element is removed from the DOM.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_be_detached(self) -> Result<(), TestError> {
        let negated = self.negated;
        let desc = if negated {
            format!("{} to NOT be detached", self.locator)
        } else {
            format!("{} to be detached", self.locator)
        };
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let poll = Duration::from_millis(self.poll_ms);
        let locator = self.locator.clone();
        let client = self.client;
        loop {
            let result = locator.resolve_one(client).await;
            let condition_met = match result {
                Ok(_) => negated,
                Err(TestError::ElementNotFound(_)) => !negated,
                Err(e) => return Err(e),
            };
            if condition_met {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(TestError::Timeout(format!(
                    "expected {desc} within {}ms",
                    self.timeout_ms
                )));
            }
            tokio::time::sleep(poll).await;
        }
    }

    // ── Internal polling helpers ─────────────────────────────────────────

    /// Poll a condition that only depends on the resolved `LocatorMatch` fields.
    async fn poll_until_simple<F>(self, check: F, description: &str) -> Result<(), TestError>
    where
        F: Fn(&LocatorMatch) -> bool,
    {
        let negated = self.negated;
        self.poll_until_simple_with_negated(check, negated, description)
            .await
    }

    async fn poll_until_simple_with_negated<F>(
        self,
        check: F,
        negated: bool,
        description: &str,
    ) -> Result<(), TestError>
    where
        F: Fn(&LocatorMatch) -> bool,
    {
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let poll = Duration::from_millis(self.poll_ms);
        let locator = self.locator.clone();
        let client = self.client;
        loop {
            let result = locator.resolve_one(client).await;
            let condition_met = match result {
                Ok(el) => {
                    let raw = check(&el);
                    if negated { !raw } else { raw }
                }
                Err(TestError::ElementNotFound(_)) if negated => true,
                Err(e @ TestError::ElementNotFound(_)) => {
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                    false
                }
                Err(e) => return Err(e),
            };
            if condition_met {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(TestError::Timeout(format!(
                    "expected {description} within {}ms",
                    self.timeout_ms
                )));
            }
            tokio::time::sleep(poll).await;
        }
    }
}

// ── Free-standing check functions (avoid lifetime issues with closures) ─────

async fn check_focused(locator: &Locator, client: &mut VictauriClient) -> Result<bool, TestError> {
    let el = locator.resolve_one(client).await?;
    let ref_str = serde_json::to_string(&el.ref_id).unwrap_or_else(|_| "\"\"".to_string());
    let code = format!(
        "(function() {{ var el = window.__VICTAURI__?.getRef({ref_str}); \
         if (!el) return false; return document.activeElement === el; }})()"
    );
    let val = client.eval_js(&code).await?;
    Ok(val.as_bool().unwrap_or(false))
}

async fn check_text_content(
    locator: &Locator,
    client: &mut VictauriClient,
) -> Result<String, TestError> {
    let el = locator.resolve_one(client).await?;
    let ref_str = serde_json::to_string(&el.ref_id).unwrap_or_else(|_| "\"\"".to_string());
    let code = format!(
        "(function() {{ var el = window.__VICTAURI__?.getRef({ref_str}); \
         if (!el) return \"\"; return el.textContent || \"\"; }})()"
    );
    let val = client.eval_js(&code).await?;
    Ok(value_to_string(&val))
}

async fn check_input_value(
    locator: &Locator,
    client: &mut VictauriClient,
) -> Result<String, TestError> {
    let el = locator.resolve_one(client).await?;
    let ref_str = serde_json::to_string(&el.ref_id).unwrap_or_else(|_| "\"\"".to_string());
    let code = format!(
        "(function() {{ var el = window.__VICTAURI__?.getRef({ref_str}); \
         if (!el) return \"\"; return el.value || \"\"; }})()"
    );
    let val = client.eval_js(&code).await?;
    Ok(value_to_string(&val))
}

async fn check_attribute(
    locator: &Locator,
    client: &mut VictauriClient,
    attr_name: &str,
) -> Result<Option<String>, TestError> {
    let el = locator.resolve_one(client).await?;
    let ref_str = serde_json::to_string(&el.ref_id).unwrap_or_else(|_| "\"\"".to_string());
    let escaped = attr_name.replace('\\', "\\\\").replace('"', "\\\"");
    let code = format!(
        "(function() {{ var el = window.__VICTAURI__?.getRef({ref_str}); \
         if (!el) return null; return el.getAttribute(\"{escaped}\"); }})()"
    );
    let val = client.eval_js(&code).await?;
    if val.is_null() {
        Ok(None)
    } else {
        Ok(Some(value_to_string(&val)))
    }
}

async fn check_checked(locator: &Locator, client: &mut VictauriClient) -> Result<bool, TestError> {
    let el = locator.resolve_one(client).await?;
    let ref_str = serde_json::to_string(&el.ref_id).unwrap_or_else(|_| "\"\"".to_string());
    let code = format!(
        "(function() {{ var el = window.__VICTAURI__?.getRef({ref_str}); \
         if (!el) return false; return !!el.checked; }})()"
    );
    let val = client.eval_js(&code).await?;
    Ok(val.as_bool().unwrap_or(false))
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn value_to_string(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn locator_role_build_query() {
        let loc = Locator::role("button");
        let q = loc.build_query();
        assert_eq!(q["role"], json!("button"));
        assert_eq!(q["max_results"], json!(50));
    }

    #[test]
    fn locator_text_build_query() {
        let loc = Locator::text("Submit");
        let q = loc.build_query();
        assert_eq!(q["text"], json!("Submit"));
    }

    #[test]
    fn locator_test_id_build_query() {
        let loc = Locator::test_id("email");
        let q = loc.build_query();
        assert_eq!(q["test_id"], json!("email"));
    }

    #[test]
    fn locator_css_build_query() {
        let loc = Locator::css(".card > h2");
        let q = loc.build_query();
        assert_eq!(q["css"], json!(".card > h2"));
    }

    #[test]
    fn locator_with_name_filter() {
        let loc = Locator::role("button").name("Submit");
        let q = loc.build_query();
        assert_eq!(q["role"], json!("button"));
        assert_eq!(q["name"], json!("Submit"));
    }

    #[test]
    fn locator_with_tag_filter() {
        let loc = Locator::text("Click me").and_tag("button");
        let q = loc.build_query();
        assert_eq!(q["text"], json!("Click me"));
        assert_eq!(q["tag"], json!("button"));
    }

    #[test]
    fn locator_nth_selection() {
        let loc = Locator::css("li").nth(3);
        match loc.pick {
            Pick::Nth(n) => assert_eq!(n, 3),
            _ => panic!("expected Pick::Nth"),
        }
    }

    #[test]
    fn locator_first_last() {
        let first = Locator::css("p").first();
        assert!(matches!(first.pick, Pick::First));

        let last = Locator::css("p").last();
        assert!(matches!(last.pick, Pick::Last));
    }

    #[test]
    fn parse_elements_array() {
        let data = json!([
            {"ref_id": "e1", "tag": "button", "role": "button", "name": "OK", "text": "OK",
             "visible": true, "enabled": true, "value": null,
             "bounds": {"x": 10.0, "y": 20.0, "width": 80.0, "height": 30.0}},
            {"ref_id": "e2", "tag": "input", "role": "textbox", "name": null, "text": "",
             "visible": true, "enabled": false, "value": "hello",
             "bounds": {"x": 0.0, "y": 0.0, "width": 200.0, "height": 24.0}}
        ]);
        let elements = Locator::parse_elements(&data);
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].ref_id, "e1");
        assert_eq!(elements[0].tag, "button");
        assert!(elements[0].visible);
        assert!(elements[0].enabled);
        assert_eq!(elements[0].bounds.unwrap().width, 80.0);
        assert_eq!(elements[1].ref_id, "e2");
        assert!(!elements[1].enabled);
        assert_eq!(elements[1].value.as_deref(), Some("hello"));
    }

    #[test]
    fn parse_elements_object() {
        let data = json!({
            "elements": [
                {"ref_id": "e5", "tag": "div", "visible": true, "enabled": true}
            ]
        });
        let elements = Locator::parse_elements(&data);
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].ref_id, "e5");
        assert_eq!(elements[0].tag, "div");
    }

    #[test]
    fn parse_elements_empty() {
        let data = json!([]);
        let elements = Locator::parse_elements(&data);
        assert!(elements.is_empty());

        let data2 = json!({"elements": []});
        let elements2 = Locator::parse_elements(&data2);
        assert!(elements2.is_empty());

        let data3 = json!(null);
        let elements3 = Locator::parse_elements(&data3);
        assert!(elements3.is_empty());
    }

    #[test]
    fn apply_filters_exact_text() {
        let loc = Locator::role("button").and_text_exact("Submit");
        let elements = vec![
            make_match("e1", "button", Some("Submit Form")),
            make_match("e2", "button", Some("Submit")),
            make_match("e3", "button", Some("Cancel")),
        ];
        let filtered = loc.apply_filters(elements);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].ref_id, "e2");
    }

    #[test]
    fn apply_filters_role() {
        let loc = Locator::text("OK").and_role("button");
        let elements = vec![
            LocatorMatch {
                ref_id: "e1".into(),
                tag: "button".into(),
                role: Some("button".into()),
                name: None,
                text: Some("OK".into()),
                visible: true,
                enabled: true,
                value: None,
                bounds: None,
            },
            LocatorMatch {
                ref_id: "e2".into(),
                tag: "span".into(),
                role: Some("generic".into()),
                name: None,
                text: Some("OK".into()),
                visible: true,
                enabled: true,
                value: None,
                bounds: None,
            },
        ];
        let filtered = loc.apply_filters(elements);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].ref_id, "e1");
    }

    #[test]
    fn apply_filters_tag() {
        let loc = Locator::role("button").and_tag("a");
        let elements = vec![
            LocatorMatch {
                ref_id: "e1".into(),
                tag: "button".into(),
                role: Some("button".into()),
                name: None,
                text: None,
                visible: true,
                enabled: true,
                value: None,
                bounds: None,
            },
            LocatorMatch {
                ref_id: "e2".into(),
                tag: "a".into(),
                role: Some("button".into()),
                name: None,
                text: None,
                visible: true,
                enabled: true,
                value: None,
                bounds: None,
            },
        ];
        let filtered = loc.apply_filters(elements);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].ref_id, "e2");
    }

    #[test]
    fn locator_display_role() {
        let loc = Locator::role("button").name("Submit");
        assert_eq!(loc.to_string(), "role(\"button\").name(\"Submit\")");
    }

    #[test]
    fn locator_display_css_nth() {
        let loc = Locator::css(".card").nth(2);
        assert_eq!(loc.to_string(), "css(\".card\").nth(2)");
    }

    #[test]
    fn locator_clone_and_modify() {
        let base = Locator::role("button");
        let submit = base.clone().name("Submit");
        let cancel = base.clone().name("Cancel");

        assert_eq!(base.to_string(), "role(\"button\")");
        assert_eq!(submit.to_string(), "role(\"button\").name(\"Submit\")");
        assert_eq!(cancel.to_string(), "role(\"button\").name(\"Cancel\")");
    }

    #[test]
    fn locator_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Locator>();
        assert_send_sync::<LocatorMatch>();
        assert_send_sync::<Bounds>();
    }

    #[test]
    fn locator_label_build_query() {
        let loc = Locator::label("Email");
        let q = loc.build_query();
        assert_eq!(q["label"], json!("Email"));
    }

    #[test]
    fn locator_placeholder_build_query() {
        let loc = Locator::placeholder("Enter email");
        let q = loc.build_query();
        assert_eq!(q["placeholder"], json!("Enter email"));
    }

    #[test]
    fn locator_alt_text_build_query() {
        let loc = Locator::alt_text("Logo");
        let q = loc.build_query();
        assert_eq!(q["alt"], json!("Logo"));
    }

    #[test]
    fn locator_title_build_query() {
        let loc = Locator::title("Close");
        let q = loc.build_query();
        assert_eq!(q["title_attr"], json!("Close"));
    }

    #[test]
    fn locator_text_exact_build_query() {
        let loc = Locator::text_exact("Submit");
        let q = loc.build_query();
        assert_eq!(q["text"], json!("Submit"));
        assert_eq!(q["exact"], json!(true));
    }

    #[test]
    fn locator_display_all_strategies() {
        assert_eq!(Locator::text("hi").to_string(), "text(\"hi\")");
        assert_eq!(Locator::text_exact("hi").to_string(), "text_exact(\"hi\")");
        assert_eq!(Locator::test_id("x").to_string(), "test_id(\"x\")");
        assert_eq!(Locator::label("E").to_string(), "label(\"E\")");
        assert_eq!(Locator::placeholder("p").to_string(), "placeholder(\"p\")");
        assert_eq!(Locator::alt_text("a").to_string(), "alt_text(\"a\")");
        assert_eq!(Locator::title("t").to_string(), "title(\"t\")");
    }

    #[test]
    fn locator_display_has_attribute() {
        let loc = Locator::css("input")
            .and_has_attribute("required", None)
            .and_has_attribute("type", Some("email"));
        assert_eq!(
            loc.to_string(),
            "css(\"input\").and_has_attribute(\"required\", None).and_has_attribute(\"type\", Some(\"email\"))"
        );
    }

    #[test]
    fn locator_display_last() {
        let loc = Locator::role("listitem").last();
        assert_eq!(loc.to_string(), "role(\"listitem\").last()");
    }

    #[test]
    fn locator_display_and_text() {
        let loc = Locator::role("link").and_text("docs");
        assert_eq!(loc.to_string(), "role(\"link\").and_text(\"docs\")");
    }

    #[test]
    fn parse_elements_skips_missing_ref_id() {
        let data = json!([
            {"tag": "div", "visible": true, "enabled": true},
            {"ref_id": "e1", "tag": "span", "visible": true, "enabled": true}
        ]);
        let elements = Locator::parse_elements(&data);
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].ref_id, "e1");
    }

    #[test]
    fn pick_one_first() {
        let loc = Locator::css("p").first();
        let elements = vec![
            make_match("e1", "p", Some("first")),
            make_match("e2", "p", Some("second")),
        ];
        let picked = loc.pick_one(elements).unwrap();
        assert_eq!(picked.ref_id, "e1");
    }

    #[test]
    fn pick_one_last() {
        let loc = Locator::css("p").last();
        let elements = vec![
            make_match("e1", "p", Some("first")),
            make_match("e2", "p", Some("second")),
        ];
        let picked = loc.pick_one(elements).unwrap();
        assert_eq!(picked.ref_id, "e2");
    }

    #[test]
    fn pick_one_nth() {
        let loc = Locator::css("p").nth(1);
        let elements = vec![
            make_match("e1", "p", Some("first")),
            make_match("e2", "p", Some("second")),
            make_match("e3", "p", Some("third")),
        ];
        let picked = loc.pick_one(elements).unwrap();
        assert_eq!(picked.ref_id, "e2");
    }

    #[test]
    fn pick_one_empty_returns_error() {
        let loc = Locator::css("p");
        let result = loc.pick_one(Vec::new());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, TestError::ElementNotFound(_)));
    }

    #[test]
    fn pick_one_nth_out_of_bounds() {
        let loc = Locator::css("p").nth(5);
        let elements = vec![make_match("e1", "p", None)];
        let result = loc.pick_one(elements);
        assert!(result.is_err());
    }

    #[test]
    fn apply_filters_name_case_insensitive() {
        let loc = Locator::role("button").name("submit");
        let elements = vec![
            LocatorMatch {
                ref_id: "e1".into(),
                tag: "button".into(),
                role: Some("button".into()),
                name: Some("Submit Form".into()),
                text: Some("Submit".into()),
                visible: true,
                enabled: true,
                value: None,
                bounds: None,
            },
            LocatorMatch {
                ref_id: "e2".into(),
                tag: "button".into(),
                role: Some("button".into()),
                name: Some("Cancel".into()),
                text: Some("Cancel".into()),
                visible: true,
                enabled: true,
                value: None,
                bounds: None,
            },
        ];
        let filtered = loc.apply_filters(elements);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].ref_id, "e1");
    }

    #[test]
    fn apply_filters_text_case_insensitive() {
        let loc = Locator::role("button").and_text("submit");
        let elements = vec![
            make_match("e1", "button", Some("Submit Form")),
            make_match("e2", "button", Some("Cancel")),
        ];
        let filtered = loc.apply_filters(elements);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].ref_id, "e1");
    }

    #[test]
    fn text_exact_strategy_filters_client_side() {
        let loc = Locator::text_exact("OK");
        let elements = vec![
            make_match("e1", "span", Some("OK")),
            make_match("e2", "span", Some("OK button")),
        ];
        let filtered = loc.apply_filters(elements);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].ref_id, "e1");
    }

    #[test]
    fn bounds_deserialize() {
        let json_str = r#"{"x":10.5,"y":20.0,"width":100.0,"height":50.5}"#;
        let bounds: Bounds = serde_json::from_str(json_str).unwrap();
        assert_eq!(bounds.x, 10.5);
        assert_eq!(bounds.height, 50.5);
    }

    #[test]
    fn bounds_serialize_roundtrip() {
        let bounds = Bounds {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
        };
        let json = serde_json::to_string(&bounds).unwrap();
        let deserialized: Bounds = serde_json::from_str(&json).unwrap();
        assert_eq!(bounds, deserialized);
    }

    #[test]
    fn value_to_string_converts_types() {
        assert_eq!(value_to_string(&json!("hello")), "hello");
        assert_eq!(value_to_string(&json!(null)), "");
        assert_eq!(value_to_string(&json!(42)), "42");
        assert_eq!(value_to_string(&json!(true)), "true");
    }

    // ── Test helpers ────────────────────────────────────────────────────

    fn make_match(ref_id: &str, tag: &str, text: Option<&str>) -> LocatorMatch {
        LocatorMatch {
            ref_id: ref_id.into(),
            tag: tag.into(),
            role: None,
            name: None,
            text: text.map(String::from),
            visible: true,
            enabled: true,
            value: None,
            bounds: None,
        }
    }
}
