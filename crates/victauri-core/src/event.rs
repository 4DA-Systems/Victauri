//! Application event types and a thread-safe ring-buffer event log.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single Tauri IPC call with timing, result, and source webview.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct IpcCall {
    /// Unique call identifier for correlation.
    pub id: String,
    /// Name of the Tauri command that was invoked.
    pub command: String,
    /// When the call was initiated.
    pub timestamp: DateTime<Utc>,
    /// Round-trip duration in milliseconds, if completed.
    pub duration_ms: Option<u64>,
    /// Current outcome of the call (pending, ok, or error).
    pub result: IpcResult,
    /// Size of the serialized arguments in bytes.
    pub arg_size_bytes: usize,
    /// Label of the webview that initiated the call.
    pub webview_label: String,
}

/// Outcome of an IPC call: pending, success with a JSON value, or error.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum IpcResult {
    /// Call is still in flight, awaiting a response.
    Pending,
    /// Call completed successfully with a JSON return value.
    Ok(serde_json::Value),
    /// Call failed with an error message.
    Err(String),
}

impl fmt::Display for IpcResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => f.write_str("pending"),
            Self::Ok(_) => f.write_str("ok"),
            Self::Err(msg) => write!(f, "error: {msg}"),
        }
    }
}

impl fmt::Display for IpcCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}] \u{2192} {}", self.command, self.id, self.result)
    }
}

impl From<IpcCall> for AppEvent {
    fn from(call: IpcCall) -> Self {
        Self::Ipc(call)
    }
}

/// The kind of user interaction captured from the DOM.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum InteractionKind {
    /// Mouse click on an element.
    Click,
    /// Double-click on an element.
    DoubleClick,
    /// Text typed into an input field.
    Fill,
    /// Individual key press event.
    KeyPress,
    /// Option selected from a dropdown.
    Select,
    /// Page navigation (URL change).
    Navigate,
    /// Scroll to element or position.
    Scroll,
}

impl fmt::Display for InteractionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Click => f.write_str("click"),
            Self::DoubleClick => f.write_str("double_click"),
            Self::Fill => f.write_str("fill"),
            Self::KeyPress => f.write_str("key_press"),
            Self::Select => f.write_str("select"),
            Self::Navigate => f.write_str("navigate"),
            Self::Scroll => f.write_str("scroll"),
        }
    }
}

/// Application event captured by the introspection layer.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum AppEvent {
    /// An IPC call between webview and Rust backend.
    Ipc(IpcCall),
    /// A change to application state in the backend.
    StateChange {
        /// State key that changed.
        key: String,
        /// When the change occurred.
        timestamp: DateTime<Utc>,
        /// Command or action that triggered the change, if known.
        caused_by: Option<String>,
    },
    /// A batch of DOM mutations observed in a webview.
    DomMutation {
        /// Webview where the mutations were observed.
        webview_label: String,
        /// When the mutations were observed.
        timestamp: DateTime<Utc>,
        /// Number of individual DOM mutations in this batch.
        mutation_count: u32,
    },
    /// A user interaction captured from the DOM during recording.
    DomInteraction {
        /// What kind of interaction occurred.
        action: InteractionKind,
        /// Best available selector for the target element (data-testid, id, CSS path).
        selector: String,
        /// Value associated with the interaction (typed text, selected option, URL, key name).
        value: Option<String>,
        /// When the interaction occurred.
        timestamp: DateTime<Utc>,
        /// Label of the webview where the interaction happened.
        webview_label: String,
    },
    /// A native window lifecycle event (e.g. focus, resize, close).
    WindowEvent {
        /// Tauri window label that emitted the event.
        label: String,
        /// Event name (e.g. "focus", "resize").
        event: String,
        /// When the event occurred.
        timestamp: DateTime<Utc>,
    },
}

impl AppEvent {
    /// Returns the timestamp of this event, regardless of variant.
    #[must_use]
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            Self::Ipc(call) => call.timestamp,
            Self::StateChange { timestamp, .. }
            | Self::DomMutation { timestamp, .. }
            | Self::DomInteraction { timestamp, .. }
            | Self::WindowEvent { timestamp, .. } => *timestamp,
        }
    }
}

/// Thread-safe ring-buffer event log. Automatically evicts the oldest events
/// when capacity is reached. All operations recover from mutex poisoning.
#[derive(Debug, Clone)]
pub struct EventLog {
    events: Arc<Mutex<VecDeque<AppEvent>>>,
    max_capacity: usize,
}

impl EventLog {
    /// Creates a new event log with the given maximum capacity.
    ///
    /// ```
    /// use victauri_core::EventLog;
    ///
    /// let log = EventLog::new(100);
    /// assert!(log.is_empty());
    /// assert_eq!(log.capacity(), 100);
    /// ```
    #[must_use]
    pub fn new(max_capacity: usize) -> Self {
        Self {
            events: Arc::new(Mutex::new(VecDeque::with_capacity(max_capacity))),
            max_capacity,
        }
    }

    /// Returns the maximum number of events this log can hold.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.max_capacity
    }

    /// Appends an event, evicting the oldest if at capacity.
    ///
    /// # Examples
    ///
    /// ```
    /// use victauri_core::{EventLog, AppEvent};
    /// use chrono::Utc;
    ///
    /// let log = EventLog::new(100);
    /// log.push(AppEvent::StateChange {
    ///     key: "theme".to_string(),
    ///     timestamp: Utc::now(),
    ///     caused_by: None,
    /// });
    /// assert_eq!(log.len(), 1);
    /// assert_eq!(log.snapshot().len(), 1);
    /// ```
    pub fn push(&self, event: AppEvent) {
        let mut events = crate::acquire_lock(&self.events, "EventLog");
        if events.len() >= self.max_capacity {
            events.pop_front();
        }
        events.push_back(event);
    }

    /// Returns a clone of all events currently in the log.
    #[must_use]
    pub fn snapshot(&self) -> Vec<AppEvent> {
        crate::acquire_lock(&self.events, "EventLog")
            .iter()
            .cloned()
            .collect()
    }

    /// Returns a paginated slice of events starting at `offset`, up to `limit` items.
    #[must_use]
    pub fn snapshot_range(&self, offset: usize, limit: usize) -> Vec<AppEvent> {
        crate::acquire_lock(&self.events, "EventLog")
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect()
    }

    /// Returns all events with a timestamp at or after the given time.
    #[must_use]
    pub fn since(&self, timestamp: DateTime<Utc>) -> Vec<AppEvent> {
        crate::acquire_lock(&self.events, "EventLog")
            .iter()
            .filter(|e| e.timestamp() >= timestamp)
            .cloned()
            .collect()
    }

    /// Returns all IPC call events, filtering out non-IPC events.
    ///
    /// # Examples
    ///
    /// ```
    /// use victauri_core::{EventLog, AppEvent, IpcCall, IpcResult};
    /// use chrono::Utc;
    ///
    /// let log = EventLog::new(100);
    /// log.push(AppEvent::Ipc(IpcCall {
    ///     id: "c1".to_string(),
    ///     command: "greet".to_string(),
    ///     timestamp: Utc::now(),
    ///     duration_ms: Some(5),
    ///     result: IpcResult::Ok(serde_json::json!("hi")),
    ///     arg_size_bytes: 0,
    ///     webview_label: "main".to_string(),
    /// }));
    /// assert_eq!(log.ipc_calls().len(), 1);
    /// ```
    #[must_use]
    pub fn ipc_calls(&self) -> Vec<IpcCall> {
        crate::acquire_lock(&self.events, "EventLog")
            .iter()
            .filter_map(|e| match e {
                AppEvent::Ipc(call) => Some(call.clone()),
                _ => None,
            })
            .collect()
    }

    /// Returns IPC calls with a timestamp at or after the given time.
    #[must_use]
    pub fn ipc_calls_since(&self, timestamp: DateTime<Utc>) -> Vec<IpcCall> {
        crate::acquire_lock(&self.events, "EventLog")
            .iter()
            .filter_map(|e| match e {
                AppEvent::Ipc(call) if call.timestamp >= timestamp => Some(call.clone()),
                _ => None,
            })
            .collect()
    }

    /// Returns the number of events currently in the log.
    #[must_use]
    pub fn len(&self) -> usize {
        crate::acquire_lock(&self.events, "EventLog")
            .len()
    }

    /// Returns true if the log contains no events.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        crate::acquire_lock(&self.events, "EventLog")
            .is_empty()
    }

    /// Removes all events from the log.
    pub fn clear(&self) {
        crate::acquire_lock(&self.events, "EventLog")
            .clear();
    }
}
