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
#[non_exhaustive]
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
#[non_exhaustive]
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
///
/// # What a locator sees
///
/// - **Not strict.** Unlike Playwright's locators, several matches are not an error: the first is
///   used unless [`nth`](Self::nth) or [`last`](Self::last) says otherwise.
/// - **Actions and `to_be_visible`/`to_be_hidden`** pick among elements that are not hidden by
///   `display` or `visibility`, so `.click()` skips a hidden duplicate.
/// - **Reads, `count`, `all` and the other expectations** see everything in the DOM, hidden or not:
///   a `display:none` element is still attached. So a position can differ between the two
///   (`nth(1).click()` is the second *visible* match, `all()[1]` the second match).
/// - **More than 1000 matches is an error**, never a truncated count.
///
/// # Example
///
/// ```rust,ignore
/// use victauri_test::locator::Locator;
///
/// // Find by role + filter by text, then click
/// let btn = Locator::role("button").and_text("Save");
/// btn.click(&mut client).await.unwrap();
///
/// // Find by test ID, fill an input
/// Locator::test_id("email-input")
///     .fill(&mut client, "user@example.com")
///     .await
///     .unwrap();
///
/// // Chain an expectation
/// Locator::css(".toast").expect(&mut client).to_be_visible().await.unwrap();
/// ```
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

    /// Locate the deepest elements whose full text contains `text` (case-insensitive substring):
    /// `<main><button>Save</button></main>` matches the button, not `<main>`.
    ///
    /// A heading and a button can both say "Save", and the first in the document wins: narrow it
    /// with `Locator::role("button").and_text("Save")` when you mean the button.
    #[must_use]
    pub fn text(text: &str) -> Self {
        Self {
            strategy: Strategy::Text(text.to_string()),
            filters: Vec::new(),
            pick: Pick::First,
        }
    }

    /// Locate the deepest elements whose whitespace-normalised full text equals `text`.
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

    /// Further filter by the presence (and, with `Some`, the exact value) of an HTML attribute.
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

    /// Select the first match (default behavior, explicit for readability). Locators are not
    /// strict, so this is also what happens when several elements match and nothing says which.
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

    /// Focus the resolved element, then press a keyboard key on it (e.g. `"Enter"`,
    /// `"Control+c"`).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches, and
    /// [`TestError::Assertion`] if the element could not be focused: the key would otherwise go to
    /// whatever holds focus.
    pub async fn press_key(
        &self,
        client: &mut VictauriClient,
        key: &str,
    ) -> Result<Value, TestError> {
        let el = self.resolve_one(client).await?;
        let focused = client.focus(&el.ref_id).await?;
        if focused.get("ok").and_then(Value::as_bool) == Some(false) {
            return Err(TestError::Assertion(format!(
                "{self}: could not focus the element to press {key}: {}",
                crate::terminal::single_line(&focused.to_string())
            )));
        }
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

    /// Check a checkbox or radio button.
    ///
    /// Toggles it with a real `click()` when it is not already checked, so the page
    /// sees the `click`/`input`/`change` sequence a user produces. (Assigning
    /// `el.checked` is invisible to React-controlled inputs: React's value tracker
    /// sees no change and swallows the synthetic `change` event.)
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches or it went
    /// stale, and [`TestError::Assertion`] if it did not end up checked (disabled,
    /// or a handler prevented the change).
    pub async fn check(&self, client: &mut VictauriClient) -> Result<Value, TestError> {
        self.set_checked(client, true).await
    }

    /// Uncheck a checkbox (toggled the same way as [`check`](Self::check)).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches or it went
    /// stale, and [`TestError::Assertion`] for a checked radio button (a user cannot
    /// uncheck one either) or if it did not end up unchecked.
    pub async fn uncheck(&self, client: &mut VictauriClient) -> Result<Value, TestError> {
        self.set_checked(client, false).await
    }

    async fn set_checked(
        &self,
        client: &mut VictauriClient,
        want: bool,
    ) -> Result<Value, TestError> {
        let el = self.resolve_one(client).await?;
        let code = element_script(&el.ref_id, &set_checked_js(want));
        element_result(client.eval_js(&code).await?, self)
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

    /// Whether the resolved element is visible: chosen among the elements that are not hidden, as
    /// [`to_be_visible`](LocatorExpect::to_be_visible) chooses it, so a hidden duplicate ahead of a
    /// visible match does not make this `false`.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn is_visible(&self, client: &mut VictauriClient) -> Result<bool, TestError> {
        match self.resolve_one(client).await {
            Ok(el) => Ok(el.visible),
            Err(TestError::ElementNotFound(_)) => {
                // None visible: `false` if it exists at all.
                self.resolve_attached(client).await.map(|_| false)
            }
            Err(e) => Err(e),
        }
    }

    /// Whether the resolved element is enabled (not disabled).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn is_enabled(&self, client: &mut VictauriClient) -> Result<bool, TestError> {
        let el = self.resolve_attached(client).await?;
        Ok(el.enabled)
    }

    /// Whether the resolved element is checked (checkbox/radio).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches.
    pub async fn is_checked(&self, client: &mut VictauriClient) -> Result<bool, TestError> {
        let body = format!("{CHECKED_STATE_JS} return isOn();");
        let val = self.eval_on_element(client, &body).await?;
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
        let el = self.resolve_attached(client).await?;
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
        // A JSON string is a valid JS string literal for any input (newlines, U+2028…).
        let name = serde_json::to_string(attr_name).unwrap_or_else(|_| "\"\"".to_string());
        let js_body = format!("return el.getAttribute({name});");
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
            let code = element_script(&el.ref_id, "return el.textContent || \"\";");
            let val = element_result(client.eval_js(&code).await?, self)?;
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

    /// Every match in the DOM, hidden or not.
    async fn resolve_all(
        &self,
        client: &mut VictauriClient,
    ) -> Result<Vec<LocatorMatch>, TestError> {
        self.find(client, true).await
    }

    async fn find(
        &self,
        client: &mut VictauriClient,
        include_hidden: bool,
    ) -> Result<Vec<LocatorMatch>, TestError> {
        let mut query = self.build_query();
        if include_hidden {
            query["include_hidden"] = json!(true);
        }
        let result = client.find_elements(query).await?;
        let elements = self.parse_answer(&result)?;
        // The query asked for one more than the cap, so this means "some were cut off": every
        // count, position and filter below would be silently wrong.
        if elements.len() > MAX_MATCHES {
            return Err(TestError::Other(format!(
                "{self}: more than {MAX_MATCHES} elements match; narrow the locator"
            )));
        }
        let elements = self.apply_filters(elements);
        self.apply_page_filters(client, elements).await
    }

    /// The element an action targets: chosen among the elements that are not hidden.
    async fn resolve_one(&self, client: &mut VictauriClient) -> Result<LocatorMatch, TestError> {
        let visible = self.find(client, false).await?;
        self.pick_one(visible)
    }

    /// The element a read or a state expectation targets: chosen among everything in the DOM.
    async fn resolve_attached(
        &self,
        client: &mut VictauriClient,
    ) -> Result<LocatorMatch, TestError> {
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
                query["text_exact"] = json!(t);
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

        // Each query key holds one condition, so the first filter for a key goes to the bridge
        // (which applies it before its cap); a later one must narrow, never replace, what is
        // already there, and is applied by `apply_filters` / `apply_page_filters`.
        for filter in &self.filters {
            let (key, value) = match filter {
                Filter::Role(r) => ("role", r),
                Filter::Name(n) => ("name", n),
                Filter::Tag(t) => ("tag", t),
                Filter::Text(t) => ("text", t),
                Filter::TextExact(t) => ("text_exact", t),
                Filter::HasAttribute(a, _) => ("attribute", a),
            };
            if query.get(key).is_some() {
                continue;
            }
            query[key] = json!(value);
            if let Filter::HasAttribute(_, Some(v)) = filter {
                query["attribute_value"] = json!(v);
            }
        }

        query["max_results"] = json!(MAX_MATCHES + 1);
        query
    }

    /// Filters whose whole answer is in the bridge's result fields.
    fn apply_filters(&self, elements: Vec<LocatorMatch>) -> Vec<LocatorMatch> {
        let mut result = elements;

        for filter in &self.filters {
            match filter {
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
                    result.retain(|el| el.tag.eq_ignore_ascii_case(expected));
                }
                // Not decidable here: the result's `text` is cut at 100 characters and attributes
                // are not in it at all. See `page_conditions`.
                Filter::Text(_) | Filter::TextExact(_) | Filter::HasAttribute(_, _) => {}
            }
        }

        result
    }

    /// JS conditions on `el` that must be checked in the page: a later text filter, which has no
    /// query key left, plus every exact-text and attribute condition. Those two are re-checked even
    /// though the query carries them, because a plugin older than these query keys ignores them and
    /// would hand back every element, and the first of those would then be clicked.
    fn page_conditions(&self) -> Vec<String> {
        let js = |text: &str| serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
        let contains = |text: &str| {
            format!(
                "norm(full(el)).toLowerCase().indexOf(norm({}).toLowerCase()) !== -1",
                js(text)
            )
        };
        let equals = |text: &str| format!("norm(full(el)) === norm({})", js(text));

        let mut conditions = Vec::new();
        let mut substring_in_query = false;
        match &self.strategy {
            Strategy::Text(_) => substring_in_query = true,
            Strategy::TextExact(t) => conditions.push(equals(t)),
            _ => {}
        }
        for filter in &self.filters {
            match filter {
                Filter::Text(t) => {
                    if substring_in_query {
                        conditions.push(contains(t));
                    }
                    substring_in_query = true;
                }
                Filter::TextExact(t) => conditions.push(equals(t)),
                Filter::HasAttribute(name, None) => {
                    conditions.push(format!("el.hasAttribute({})", js(name)));
                }
                Filter::HasAttribute(name, Some(value)) => {
                    conditions.push(format!("el.getAttribute({}) === {}", js(name), js(value)));
                }
                Filter::Role(_) | Filter::Name(_) | Filter::Tag(_) => {}
            }
        }
        conditions
    }

    /// Keeps the elements meeting [`Self::page_conditions`]; one eval for all of them.
    async fn apply_page_filters(
        &self,
        client: &mut VictauriClient,
        elements: Vec<LocatorMatch>,
    ) -> Result<Vec<LocatorMatch>, TestError> {
        let conditions = self.page_conditions();
        if conditions.is_empty() || elements.is_empty() {
            return Ok(elements);
        }
        let refs: Vec<&str> = elements.iter().map(|el| el.ref_id.as_str()).collect();
        let answer = client.eval_js(&page_filter_js(&refs, &conditions)).await?;
        let keep = answer
            .as_array()
            .filter(|keep| keep.len() == elements.len())
            .ok_or_else(|| {
                TestError::Other(format!(
                    "{self}: could not check its filters in the page, got: {}",
                    crate::terminal::single_line(&answer.to_string())
                ))
            })?;
        Ok(elements
            .into_iter()
            .zip(keep)
            .filter(|(_, keep)| keep.as_bool() == Some(true))
            .map(|(el, _)| el)
            .collect())
    }

    /// [`Self::parse_elements`], refusing an answer that is not an element list: read as an empty
    /// list, it would satisfy `to_have_count(0)` on a page where the bridge is not even loaded.
    fn parse_answer(&self, result: &Value) -> Result<Vec<LocatorMatch>, TestError> {
        if result.is_array() || result.get("elements").is_some_and(Value::is_array) {
            return Ok(Self::parse_elements(result));
        }
        Err(TestError::Other(format!(
            "{self}: find_elements did not return an element list (is the page loaded?): {}",
            crate::terminal::single_line(&result.to_string())
        )))
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
        let el = self.resolve_attached(client).await?;
        let code = element_script(&el.ref_id, js_body);
        element_result(client.eval_js(&code).await?, self)
    }
}

/// The most matches a locator will work with; asked for as this plus one, to detect more.
const MAX_MATCHES: usize = 1000;

/// JS answering, for each ref, whether its element meets all of `conditions`. Text is what the
/// bridge's `text` / `text_exact` judge: `textContent` without script and style source, whitespace
/// collapsed. A stricter reading here would drop elements the bridge rightly matched.
fn page_filter_js(refs: &[&str], conditions: &[String]) -> String {
    format!(
        "(function() {{ var refs = {}; var out = []; \
         var norm = function(s) {{ return (s || '').replace(/\\s+/g, ' ').trim(); }}; \
         var full = function(n) {{ var t = ''; var kids = n.childNodes || []; \
           for (var k = 0; k < kids.length; k++) {{ var c = kids[k]; \
             if (c.nodeType === 3) t += c.nodeValue; \
             else if (c.nodeType === 1 && !/^(SCRIPT|STYLE|NOSCRIPT|TEMPLATE)$/.test(c.tagName)) t += full(c); }} \
           return t; }}; \
         for (var i = 0; i < refs.length; i++) {{ \
           var el = window.__VICTAURI__?.getRef(refs[i]); \
           out[i] = !!el && {}; }} \
         return out; }})()",
        serde_json::to_string(refs).unwrap_or_else(|_| "[]".to_string()),
        conditions.join(" && ")
    )
}

/// Marker an [`element_script`] returns when its ref no longer resolves.
const MISSING_KEY: &str = "__victauri_missing_ref";
/// Marker an [`element_script`] returns, with a message, when its action failed.
const ERROR_KEY: &str = "__victauri_error";

/// Wraps `js_body` (which sees the element as `el`) in a script that reports a ref
/// that no longer resolves — instead of returning `null`, which callers would read
/// as an empty text, an absent attribute or an unchecked box.
pub(crate) fn element_script(ref_id: &str, js_body: &str) -> String {
    let ref_str = serde_json::to_string(ref_id).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        "(function() {{ var el = window.__VICTAURI__?.getRef({ref_str}); \
         if (!el) return {{ {MISSING_KEY}: true }}; {js_body} }})()"
    )
}

/// Turns an [`element_script`] result's markers into errors.
fn element_result(val: Value, locator: &Locator) -> Result<Value, TestError> {
    if val.get(MISSING_KEY).and_then(Value::as_bool) == Some(true) {
        return Err(TestError::ElementNotFound(format!(
            "{locator} (the element went stale before it could be read)"
        )));
    }
    if let Some(msg) = val.get(ERROR_KEY).and_then(Value::as_str) {
        // The message comes from the page: never let it drive a terminal or a CI log (R5-TERM2).
        return Err(TestError::Assertion(format!(
            "{locator}: {}",
            crate::terminal::single_line(msg)
        )));
    }
    Ok(val)
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
///
/// An element that does not exist is "not yet": it is waited for, and if it never appears the
/// assertion fails with [`TestError::ElementNotFound`], negated or not. `.not().to_have_text("x")`
/// needs an element to say something about. Only [`to_be_hidden`](Self::to_be_hidden),
/// `.not().to_be_visible()`, [`to_be_detached`](Self::to_be_detached), `.not().to_be_attached()`
/// and `to_have_count(0)` are satisfied by absence. Any other failure (a dead app, an invalid
/// selector, a tool error) is returned at once and never counts as the condition being met. A page
/// with no body yet (mid-navigation) has no answer either way: it is retried until the timeout and
/// then returned as that error, whatever the assertion.
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
        // `.not()`: gone counts as not visible.
        self.poll_until_simple(|el| el.visible, negated, negated, false, &desc)
            .await
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
        // "hidden" means visible==false, so invert the check; gone counts as hidden.
        self.poll_until_simple(|el| el.visible, !negated, !negated, false, &desc)
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
        // Being absent says nothing about being enabled.
        self.poll_until_simple(|el| el.enabled, negated, false, true, &desc)
            .await
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
        self.poll_until_simple(|el| el.enabled, !negated, false, true, &desc)
            .await
    }

    /// Wait until the element has keyboard focus.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time, and
    /// [`TestError::ElementNotFound`] if the element never appears.
    pub async fn to_be_focused(self) -> Result<(), TestError> {
        let desc = self.describe("be focused");
        self.poll_read("return document.activeElement === el;", is_true, &desc)
            .await
    }

    /// Wait until the element's text content equals `expected`.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time, and
    /// [`TestError::ElementNotFound`] if the element never appears.
    pub async fn to_have_text(self, expected: &str) -> Result<(), TestError> {
        let desc = self.describe(&format!("have text \"{expected}\""));
        self.poll_read(
            TEXT_CONTENT_JS,
            |val| value_to_string(val).trim() == expected,
            &desc,
        )
        .await
    }

    /// Wait until the element's text content contains `expected` as a substring.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time, and
    /// [`TestError::ElementNotFound`] if the element never appears.
    pub async fn to_contain_text(self, expected: &str) -> Result<(), TestError> {
        let desc = self.describe(&format!("contain text \"{expected}\""));
        self.poll_read(
            TEXT_CONTENT_JS,
            |val| value_to_string(val).contains(expected),
            &desc,
        )
        .await
    }

    /// Wait until the element's input value equals `expected`.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time,
    /// [`TestError::ElementNotFound`] if the element never appears, and [`TestError::Assertion`] if
    /// the element has no `value` (it is not a form control), negated or not.
    pub async fn to_have_value(self, expected: &str) -> Result<(), TestError> {
        let desc = self.describe(&format!("have value \"{expected}\""));
        self.poll_read(
            &value_read_js(),
            |val| value_to_string(val) == expected,
            &desc,
        )
        .await
    }

    /// Wait until the element has the given attribute with the given value.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time, and
    /// [`TestError::ElementNotFound`] if the element never appears.
    pub async fn to_have_attribute(self, attr_name: &str, value: &str) -> Result<(), TestError> {
        let desc = self.describe(&format!("have attribute {attr_name}=\"{value}\""));
        // A JSON string is a valid JS string literal for any input (newlines, U+2028…).
        let name = serde_json::to_string(attr_name).unwrap_or_else(|_| "\"\"".to_string());
        let body = format!("return el.getAttribute({name});");
        self.poll_read(&body, |val| val.as_str() == Some(value), &desc)
            .await
    }

    /// Wait until the number of matching elements (hidden ones included) equals `expected`.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time. An error from the query
    /// itself (a dead app, an invalid selector) is returned as is: it says nothing about the count.
    pub async fn to_have_count(self, expected: usize) -> Result<(), TestError> {
        let negated = self.negated;
        let desc = self.describe(&format!("have count {expected}"));
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let poll = Duration::from_millis(self.poll_ms);
        let locator = self.locator.clone();
        let client = self.client;
        loop {
            match locator.resolve_all(client).await {
                Ok(all) => {
                    if (all.len() == expected) != negated {
                        return Ok(());
                    }
                }
                Err(e) if page_not_ready(&e) => {
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
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
    /// Returns [`TestError::Timeout`] if the condition is not met in time,
    /// [`TestError::ElementNotFound`] if the element never appears, and [`TestError::Assertion`] if
    /// the element has no checked state, negated or not.
    pub async fn to_be_checked(self) -> Result<(), TestError> {
        let desc = self.describe("be checked");
        self.poll_read(&checked_read_js(), is_true, &desc).await
    }

    /// Wait until the element is unchecked.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time,
    /// [`TestError::ElementNotFound`] if the element never appears, and [`TestError::Assertion`] if
    /// the element has no checked state (a wrong element is not "unchecked"), negated or not.
    pub async fn to_be_unchecked(self) -> Result<(), TestError> {
        let desc = self.describe("be unchecked");
        self.poll_read(
            &checked_read_js(),
            |val| val.as_bool() == Some(false),
            &desc,
        )
        .await
    }

    /// Wait until the element exists in the DOM, visible or not.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_be_attached(self) -> Result<(), TestError> {
        let desc = self.describe("be attached");
        let want_attached = !self.negated;
        self.poll_attached(want_attached, &desc).await
    }

    /// Wait until the element is removed from the DOM. A hidden element is still attached.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the condition is not met in time.
    pub async fn to_be_detached(self) -> Result<(), TestError> {
        let desc = self.describe("be detached");
        let want_attached = self.negated;
        self.poll_attached(want_attached, &desc).await
    }

    // ── Internal polling helpers ─────────────────────────────────────────

    fn describe(&self, what: &str) -> String {
        if self.negated {
            format!("{} to NOT {what}", self.locator)
        } else {
            format!("{} to {what}", self.locator)
        }
    }

    async fn poll_attached(self, want_attached: bool, description: &str) -> Result<(), TestError> {
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let poll = Duration::from_millis(self.poll_ms);
        let locator = self.locator.clone();
        let client = self.client;
        loop {
            let attached = match locator.resolve_attached(client).await {
                Ok(_) => true,
                Err(TestError::ElementNotFound(_)) => false,
                Err(e) if page_not_ready(&e) => {
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                    tokio::time::sleep(poll).await;
                    continue;
                }
                Err(e) => return Err(e),
            };
            if attached == want_attached {
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

    /// Poll a condition on the resolved `LocatorMatch` fields. `absent_passes` says whether no
    /// element satisfies it, which is separate from `negated`: `to_be_disabled` negates "enabled",
    /// yet a missing element is not disabled. `include_hidden` is the scope the element is chosen
    /// from (see [`Locator`]).
    async fn poll_until_simple<F>(
        self,
        check: F,
        negated: bool,
        absent_passes: bool,
        include_hidden: bool,
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
            let found = if include_hidden {
                locator.resolve_attached(client).await
            } else {
                locator.resolve_one(client).await
            };
            let condition_met = match found {
                Ok(el) => check(&el) != negated,
                Err(TestError::ElementNotFound(_)) if absent_passes => true,
                Err(e @ TestError::ElementNotFound(_)) => {
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                    false
                }
                Err(e) if page_not_ready(&e) => {
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

    /// Poll a condition on a value read from the element by `js_body` (which sees it as `el`),
    /// honouring `.not()`. A missing element is "not yet", including a ref that went stale between
    /// finding and reading; it never reads as an empty value, which would satisfy a negation.
    async fn poll_read<F>(self, js_body: &str, check: F, description: &str) -> Result<(), TestError>
    where
        F: Fn(&Value) -> bool,
    {
        let negated = self.negated;
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let poll = Duration::from_millis(self.poll_ms);
        let locator = self.locator.clone();
        let client = self.client;
        loop {
            match locator.eval_on_element(client, js_body).await {
                Ok(val) => {
                    if check(&val) != negated {
                        return Ok(());
                    }
                }
                Err(e @ TestError::ElementNotFound(_)) => {
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                }
                Err(e) if page_not_ready(&e) => {
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
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

const TEXT_CONTENT_JS: &str = "return el.textContent || \"\";";

/// `find_elements` on a document with no body yet (a navigation in progress): no answer about any
/// element, so an expectation waits it out and it can never read as "no elements".
fn page_not_ready(error: &TestError) -> bool {
    matches!(error, TestError::ToolError(msg) if msg.starts_with("page not ready"))
}

fn is_true(val: &Value) -> bool {
    val.as_bool() == Some(true)
}

/// JS (sees the element as `el`) defining `isOn()`: the checked state of a native
/// checkbox/radio (`el.checked`), else of an ARIA checkbox/radio/switch/menuitemcheckbox
/// (`aria-checked`, as Radix/Mantine/Headless UI render them on a `<button>`), else of an
/// ARIA toggle button (`aria-pressed`). `"mixed"` counts as not checked.
const CHECKED_STATE_JS: &str = "var isOn = function() { \
     if (el.type === 'checkbox' || el.type === 'radio') return !!el.checked; \
     var aria = el.getAttribute && el.getAttribute('aria-checked'); \
     if (aria !== null && aria !== undefined) return aria === 'true'; \
     var pressed = el.getAttribute && el.getAttribute('aria-pressed'); \
     if (pressed !== null && pressed !== undefined) return pressed === 'true'; \
     return !!el.checked; };";

/// The body of [`Locator::check`] / [`Locator::uncheck`] (B-L9).
///
/// Clicks only when the state differs, like a user. A native input updates synchronously; an
/// ARIA checkbox's `aria-checked` is re-rendered by its framework, possibly after the click
/// returns (React schedules it), so the result is awaited — briefly polled — rather than read
/// once. Reading `el.checked` alone (the old code) saw `undefined` on an ARIA checkbox: `check()`
/// clicked, then reported failure, and a retry toggled it back; `uncheck()` did nothing.
fn set_checked_js(want: bool) -> String {
    let state = if want { "checked" } else { "unchecked" };
    format!(
        "{CHECKED_STATE_JS} var want = {want}; \
         var radio = el.type === 'radio' || \
           (el.getAttribute && el.getAttribute('role') === 'radio'); \
         if (!want && radio && isOn()) return {{ {ERROR_KEY}: \
           'a checked radio button cannot be unchecked; check another option' }}; \
         if (isOn() !== want) el.click(); \
         if (isOn() === want) return true; \
         return new Promise(function(resolve) {{ var tries = 0; \
           (function poll() {{ \
             if (isOn() === want) return resolve(true); \
             if (++tries > 25) return resolve({{ {ERROR_KEY}: \
               'the element did not become {state} (disabled, or a handler prevented it)' }}); \
             setTimeout(poll, 20); }})(); }});"
    )
}

/// The read behind the checked matchers. An element with no checked state is an error rather than
/// `false`, which would pass `to_be_unchecked` and `.not().to_be_checked()` on a wrong element.
fn checked_read_js() -> String {
    format!(
        "{CHECKED_STATE_JS} \
         var attr = function(n) {{ return el.getAttribute ? el.getAttribute(n) : null; }}; \
         if (el.type !== 'checkbox' && el.type !== 'radio' \
             && attr('aria-checked') === null && attr('aria-pressed') === null \
             && !/^(checkbox|radio|switch|menuitemcheckbox|menuitemradio)$/.test(attr('role') || '') \
             && !('checked' in el && el.tagName !== 'INPUT')) \
           return {{ {ERROR_KEY}: 'this element has no checked state (not a checkbox, radio or ARIA \
             toggle)' }}; \
         return isOn();"
    )
}

/// The read behind `to_have_value`. An element with no `value` is an error rather than `""`, which
/// would pass `to_have_value("")` and `.not().to_have_value("x")` on a wrong element.
fn value_read_js() -> String {
    format!(
        "if (!('value' in el)) return {{ {ERROR_KEY}: 'this element has no value (not a form \
           control)' }}; \
         return el.value === null || el.value === undefined ? \"\" : el.value;"
    )
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

    /// Runs `js_body` as an [`element_script`] in Node, once per fake element in `elements` (a JS
    /// array literal), and returns each answer as JSON.
    fn run_on_elements(js_body: &str, elements: &str) -> Option<Vec<Value>> {
        let script = element_script("e1", js_body);
        let program = format!(
            "var window = {{}}; var fakes = {elements}; fakes.forEach(function(el) {{ \
             if (el.attrs) el.getAttribute = function(n) {{ \
               return Object.prototype.hasOwnProperty.call(el.attrs, n) ? el.attrs[n] : null; }}; \
             window.__VICTAURI__ = {{ getRef: function() {{ return el; }} }}; \
             console.log(JSON.stringify({script})); }});"
        );
        let out = match std::process::Command::new("node")
            .args(["-e", &program])
            .output()
        {
            Ok(out) => out,
            Err(e) => {
                eprintln!("skipping: node is not available ({e})");
                return None;
            }
        };
        assert!(
            out.status.success(),
            "node failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect(),
        )
    }

    #[test]
    fn the_checked_matchers_refuse_an_element_with_no_checked_state() {
        let Some(answers) = run_on_elements(
            &checked_read_js(),
            r#"[
              {tagName:"INPUT",type:"checkbox",checked:false,attrs:{}},
              {tagName:"INPUT",type:"radio",checked:true,attrs:{}},
              {tagName:"BUTTON",attrs:{"aria-checked":"false"}},
              {tagName:"BUTTON",attrs:{"aria-pressed":"true"}},
              {tagName:"BUTTON",attrs:{role:"switch"}},
              {tagName:"MD-CHECKBOX",checked:false,attrs:{}},
              {tagName:"DIV",attrs:{}},
              {tagName:"INPUT",type:"text",checked:false,attrs:{}}
            ]"#,
        ) else {
            return;
        };
        assert_eq!(
            answers[..6],
            [
                json!(false),
                json!(true),
                json!(false),
                json!(true),
                json!(false),
                json!(false)
            ]
        );
        for wrong in &answers[6..] {
            assert!(wrong[ERROR_KEY].is_string(), "{wrong}");
        }
    }

    #[test]
    fn to_have_value_refuses_an_element_with_no_value() {
        let Some(answers) = run_on_elements(
            &value_read_js(),
            r#"[
              {tagName:"INPUT",value:""},
              {tagName:"INPUT",value:"typed"},
              {tagName:"LI",value:0},
              {tagName:"DIV"}
            ]"#,
        ) else {
            return;
        };
        assert_eq!(answers[..3], [json!(""), json!("typed"), json!(0)]);
        assert!(answers[3][ERROR_KEY].is_string(), "{}", answers[3]);
        assert_eq!(value_to_string(&answers[2]), "0");
    }

    #[test]
    fn locator_role_build_query() {
        let loc = Locator::role("button");
        let q = loc.build_query();
        assert_eq!(q["role"], json!("button"));
        assert_eq!(q["max_results"], json!(1001));
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
    fn an_answer_that_is_not_a_list_is_refused() {
        let loc = Locator::css("p");
        assert!(loc.parse_answer(&json!([])).unwrap().is_empty());
        assert!(
            loc.parse_answer(&json!({"elements": []}))
                .unwrap()
                .is_empty()
        );
        assert!(loc.parse_answer(&json!(null)).is_err());
        assert!(loc.parse_answer(&json!("undefined")).is_err());
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
    fn page_conditions_cover_what_the_result_fields_cannot_answer() {
        assert!(Locator::css("li").page_conditions().is_empty());
        // The one substring filter the query carries needs no second check.
        assert!(
            Locator::role("button")
                .and_text("Save")
                .page_conditions()
                .is_empty()
        );
        let two_texts = Locator::text("a").and_text("b").page_conditions();
        assert_eq!(two_texts.len(), 1);
        assert!(two_texts[0].contains("\"b\""), "{two_texts:?}");
        assert_eq!(Locator::text_exact("a").page_conditions().len(), 1);
        let attr = Locator::css("x")
            .and_has_attribute("data-a", Some("1"))
            .and_has_attribute("data-b", None)
            .page_conditions();
        assert_eq!(
            attr,
            [
                "el.getAttribute(\"data-a\") === \"1\"",
                "el.hasAttribute(\"data-b\")"
            ]
        );
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
        assert_eq!(q["text_exact"], json!("Submit"));
        assert!(q.get("text").is_none());
        assert!(q.get("exact").is_none());
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

    /// R5-TERM2: an element script's error message is page-controlled text.
    #[test]
    fn element_error_text_is_terminal_safe() {
        let val = json!({ ERROR_KEY: "nope\n::error::forged\u{1b}[2J" });
        let err = element_result(val, &Locator::css("p")).unwrap_err();
        let TestError::Assertion(msg) = &err else {
            panic!("expected an assertion error, got {err:?}");
        };
        assert!(!msg.chars().any(char::is_control), "{msg:?}");
        assert!(msg.contains("nope") && msg.contains("forged"), "{msg}");
        assert!(
            !err.to_string().lines().any(|l| l.starts_with("::")),
            "{err}"
        );
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

    // ── B-L9: check()/uncheck() on ARIA checkboxes, executed in Node ──

    /// Run `set_checked_js(want)` against fake elements in Node and return one JSON line per
    /// scenario: `{name, result, on}` (`on` = the element's state afterwards). `None` (test
    /// skipped) when `node` is not installed.
    fn run_set_checked_scenarios() -> Option<Vec<serde_json::Value>> {
        let check = set_checked_js(true);
        let uncheck = set_checked_js(false);
        let program = format!(
            r"
function aria(role, attr, initial, delayMs, disabled) {{
  var attrs = {{ role: role }}; attrs[attr] = String(initial);
  return {{
    type: undefined,
    getAttribute: function(k) {{ return Object.prototype.hasOwnProperty.call(attrs, k) ? attrs[k] : null; }},
    click: function() {{
      if (disabled) return;
      var flip = function() {{ attrs[attr] = String(attrs[attr] !== 'true'); }};
      if (delayMs) setTimeout(flip, delayMs); else flip();
    }},
    on: function() {{ return attrs[attr] === 'true'; }}
  }};
}}
function native(type, initial) {{
  var el = {{ type: type, checked: initial, getAttribute: function() {{ return null; }},
             click: function() {{ el.checked = type === 'radio' ? true : !el.checked; }},
             on: function() {{ return el.checked; }} }};
  return el;
}}
var ERR = '{ERROR_KEY}';
async function run(name, el, want) {{
  var result = await (want ? (function() {{ {check} }})() : (function() {{ {uncheck} }})());
  if (result && typeof result === 'object' && ERR in result) result = 'error: ' + result[ERR];
  console.log(JSON.stringify({{ name: name, result: result, on: el.on() }}));
}}
(async function() {{
  var el;
  el = aria('checkbox', 'aria-checked', false); await run('aria check', el, true);
  el = aria('checkbox', 'aria-checked', true); await run('aria uncheck', el, false);
  el = aria('checkbox', 'aria-checked', true); await run('aria check when already on', el, true);
  el = aria('checkbox', 'aria-checked', false, 40); await run('aria async check', el, true);
  el = aria('switch', 'aria-checked', true, 40); await run('aria async uncheck', el, false);
  el = aria('button', 'aria-pressed', false); await run('pressed toggle', el, true);
  el = aria('radio', 'aria-checked', true); await run('aria radio uncheck', el, false);
  el = aria('checkbox', 'aria-checked', false, 0, true); await run('aria disabled', el, true);
  el = native('checkbox', false); await run('native check', el, true);
  el = native('checkbox', true); await run('native uncheck', el, false);
}})();
"
        );
        let out = match std::process::Command::new("node")
            .args(["-e", &program])
            .output()
        {
            Ok(out) => out,
            Err(e) => {
                eprintln!("skipping: node is not available ({e})");
                return None;
            }
        };
        assert!(
            out.status.success(),
            "node failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|l| serde_json::from_str(l).expect("scenario JSON"))
                .collect(),
        )
    }

    /// The filter script is JS built by string: run it, against fake elements.
    #[test]
    fn page_filter_script_runs_and_selects() {
        let loc = Locator::text_exact("Save  now")
            .and_has_attribute("data-a", Some("1"))
            .and_has_attribute("data-b", None);
        let code = page_filter_js(&["e1", "e2", "e3", "gone"], &loc.page_conditions());
        let program = format!(
            r#"
function el(text, attrs) {{
  return {{ childNodes: [
              {{ nodeType: 3, nodeValue: text }},
              {{ nodeType: 1, tagName: "STYLE", childNodes: [{{ nodeType: 3, nodeValue: "p {{}}" }}] }}
            ],
           getAttribute: function(k) {{ return k in attrs ? attrs[k] : null; }},
           hasAttribute: function(k) {{ return k in attrs; }} }};
}}
var page = {{
  e1: el(" Save \n now ", {{ "data-a": "1", "data-b": "" }}),
  e2: el("Save now", {{ "data-a": "2", "data-b": "" }}),
  e3: el("Save now and later", {{ "data-a": "1", "data-b": "" }})
}};
var window = {{ __VICTAURI__: {{ getRef: function(r) {{ return page[r]; }} }} }};
console.log(JSON.stringify({code}));
"#
        );
        let out = match std::process::Command::new("node")
            .args(["-e", &program])
            .output()
        {
            Ok(out) => out,
            Err(e) => {
                eprintln!("skipping: node is not available ({e})");
                return;
            }
        };
        assert!(
            out.status.success(),
            "node failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "[true,false,false,false]"
        );
    }

    #[test]
    fn check_and_uncheck_work_on_aria_checkboxes() {
        let Some(results) = run_set_checked_scenarios() else {
            return;
        };
        let by_name = |name: &str| {
            results
                .iter()
                .find(|r| r["name"] == name)
                .unwrap_or_else(|| panic!("no scenario {name}: {results:?}"))
                .clone()
        };
        for (name, on) in [
            ("aria check", true),
            ("aria uncheck", false),
            ("aria check when already on", true),
            ("aria async check", true),
            ("aria async uncheck", false),
            ("pressed toggle", true),
            ("native check", true),
            ("native uncheck", false),
        ] {
            let r = by_name(name);
            assert_eq!(r["result"], serde_json::json!(true), "{name}: {r}");
            assert_eq!(r["on"], serde_json::json!(on), "{name}: {r}");
        }
        let radio = by_name("aria radio uncheck");
        assert!(
            radio["result"]
                .as_str()
                .is_some_and(|s| s.contains("radio")),
            "{radio}"
        );
        assert_eq!(radio["on"], serde_json::json!(true));
        let disabled = by_name("aria disabled");
        assert!(
            disabled["result"]
                .as_str()
                .is_some_and(|s| s.contains("did not become checked")),
            "{disabled}"
        );
    }
}
