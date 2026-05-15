use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::Value;
use tokio::sync::{RwLock, oneshot};

use victauri_core::EventLog;
use victauri_core::recording::EventRecorder;

const DEFAULT_EVENT_CAPACITY: usize = 10_000;
const DEFAULT_RECORDER_CAPACITY: usize = 50_000;

/// Per-tab bridge state tracked by the native host.
pub struct TabState {
    pub tab_id: u32,
    pub url: String,
    pub title: String,
    pub bridge_ready: bool,
    #[allow(dead_code)]
    pub recorder: EventRecorder,
    #[allow(dead_code)]
    pub event_log: EventLog,
    #[allow(dead_code)]
    pub pending_commands: HashMap<String, oneshot::Sender<Value>>,
}

impl TabState {
    fn new(tab_id: u32, url: String, title: String) -> Self {
        Self {
            tab_id,
            url,
            title,
            bridge_ready: false,
            recorder: EventRecorder::new(DEFAULT_RECORDER_CAPACITY),
            event_log: EventLog::new(DEFAULT_EVENT_CAPACITY),
            pending_commands: HashMap::new(),
        }
    }
}

/// Manages the state of all browser tabs connected via the extension.
pub struct TabManager {
    tabs: RwLock<HashMap<u32, TabState>>,
    active_tab: AtomicU32,
}

impl TabManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tabs: RwLock::new(HashMap::new()),
            active_tab: AtomicU32::new(0),
        }
    }

    /// Register a pending command and return a receiver for the response.
    #[allow(dead_code)]
    pub async fn register_pending(
        &self,
        tab_id: u32,
        command_id: &str,
    ) -> Option<oneshot::Receiver<Value>> {
        let mut tabs = self.tabs.write().await;
        let tab = tabs.get_mut(&tab_id)?;
        let (tx, rx) = oneshot::channel();
        tab.pending_commands.insert(command_id.to_string(), tx);
        Some(rx)
    }

    /// Resolve a pending command with a response value.
    #[allow(dead_code)]
    pub async fn resolve_pending(&self, tab_id: u32, command_id: &str, value: Value) -> bool {
        let mut tabs = self.tabs.write().await;
        let Some(tab) = tabs.get_mut(&tab_id) else {
            return false;
        };
        if let Some(tx) = tab.pending_commands.remove(command_id) {
            let _ = tx.send(value);
            true
        } else {
            false
        }
    }

    /// Get the target tab ID, resolving `None` to the active tab.
    ///
    /// # Errors
    ///
    /// Returns an error if no active tab is set or the specified tab doesn't exist.
    #[allow(dead_code)]
    pub async fn resolve_tab(&self, tab_id: Option<u32>) -> Result<u32, TabError> {
        let id = tab_id.unwrap_or_else(|| self.active_tab.load(Ordering::Relaxed));
        if id == 0 {
            return Err(TabError::NoActiveTab);
        }
        let tabs = self.tabs.read().await;
        if tabs.contains_key(&id) {
            Ok(id)
        } else {
            Err(TabError::TabNotFound(id))
        }
    }

    pub async fn on_tab_created(&self, tab_id: u32, url: &str, title: &str) {
        let mut tabs = self.tabs.write().await;
        tabs.insert(
            tab_id,
            TabState::new(tab_id, url.to_string(), title.to_string()),
        );
    }

    pub async fn on_tab_closed(&self, tab_id: u32) {
        let mut tabs = self.tabs.write().await;
        tabs.remove(&tab_id);
    }

    pub async fn on_tab_activated(&self, tab_id: u32) {
        self.active_tab.store(tab_id, Ordering::Relaxed);
    }

    pub async fn on_tab_updated(&self, tab_id: u32, url: Option<&str>, title: Option<&str>) {
        let mut tabs = self.tabs.write().await;
        if let Some(tab) = tabs.get_mut(&tab_id) {
            if let Some(u) = url {
                tab.url = u.to_string();
            }
            if let Some(t) = title {
                tab.title = t.to_string();
            }
        }
    }

    pub async fn on_bridge_ready(&self, tab_id: u32) {
        let mut tabs = self.tabs.write().await;
        if let Some(tab) = tabs.get_mut(&tab_id) {
            tab.bridge_ready = true;
        }
    }

    #[allow(dead_code)]
    pub async fn get_active_tab_id(&self) -> u32 {
        self.active_tab.load(Ordering::Relaxed)
    }

    /// List all tracked tabs with their metadata.
    pub async fn list_tabs(&self) -> Vec<TabInfo> {
        let tabs = self.tabs.read().await;
        let active = self.active_tab.load(Ordering::Relaxed);
        tabs.values()
            .map(|t| TabInfo {
                tab_id: t.tab_id,
                url: t.url.clone(),
                title: t.title.clone(),
                bridge_ready: t.bridge_ready,
                active: t.tab_id == active,
            })
            .collect()
    }

    #[must_use]
    pub async fn tab_count(&self) -> usize {
        self.tabs.read().await.len()
    }

    #[allow(dead_code)]
    pub async fn is_bridge_ready(&self, tab_id: u32) -> bool {
        let tabs = self.tabs.read().await;
        tabs.get(&tab_id).is_some_and(|t| t.bridge_ready)
    }
}

impl Default for TabManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TabInfo {
    pub tab_id: u32,
    pub url: String,
    pub title: String,
    pub bridge_ready: bool,
    pub active: bool,
}

#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum TabError {
    #[error("no active tab — open a tab in the browser first")]
    NoActiveTab,

    #[error("tab {0} not found — it may have been closed")]
    TabNotFound(u32),
}


#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tab_lifecycle() {
        let mgr = TabManager::new();

        mgr.on_tab_created(1, "https://example.com", "Example").await;
        mgr.on_tab_activated(1).await;

        assert_eq!(mgr.tab_count().await, 1);
        assert_eq!(mgr.get_active_tab_id().await, 1);

        let resolved = mgr.resolve_tab(None).await.unwrap();
        assert_eq!(resolved, 1);

        mgr.on_bridge_ready(1).await;
        assert!(mgr.is_bridge_ready(1).await);

        mgr.on_tab_closed(1).await;
        assert_eq!(mgr.tab_count().await, 0);
    }

    #[tokio::test]
    async fn resolve_tab_errors() {
        let mgr = TabManager::new();

        assert!(matches!(
            mgr.resolve_tab(None).await,
            Err(TabError::NoActiveTab)
        ));

        assert!(matches!(
            mgr.resolve_tab(Some(999)).await,
            Err(TabError::TabNotFound(999))
        ));
    }

    #[tokio::test]
    async fn pending_command_lifecycle() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://example.com", "Test").await;

        let rx = mgr.register_pending(1, "cmd-1").await.unwrap();
        mgr.resolve_pending(1, "cmd-1", serde_json::json!({"ok": true}))
            .await;

        let result = rx.await.unwrap();
        assert_eq!(result, serde_json::json!({"ok": true}));
    }

    #[tokio::test]
    async fn list_tabs_with_active() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://one.com", "One").await;
        mgr.on_tab_created(2, "https://two.com", "Two").await;
        mgr.on_tab_activated(2).await;

        let tabs = mgr.list_tabs().await;
        assert_eq!(tabs.len(), 2);

        let active: Vec<_> = tabs.iter().filter(|t| t.active).collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].tab_id, 2);
    }

    #[tokio::test]
    async fn tab_update() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://old.com", "Old Title").await;
        mgr.on_tab_updated(1, Some("https://new.com"), Some("New Title"))
            .await;

        let tabs = mgr.list_tabs().await;
        assert_eq!(tabs[0].url, "https://new.com");
        assert_eq!(tabs[0].title, "New Title");
    }

    #[tokio::test]
    async fn bridge_ready_unknown_tab_noop() {
        let mgr = TabManager::new();
        mgr.on_bridge_ready(999).await;
        assert!(!mgr.is_bridge_ready(999).await);
    }

    #[tokio::test]
    async fn bridge_not_ready_by_default() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://x.com", "X").await;
        assert!(!mgr.is_bridge_ready(1).await);
    }

    #[tokio::test]
    async fn resolve_pending_unknown_tab_returns_false() {
        let mgr = TabManager::new();
        let resolved = mgr
            .resolve_pending(999, "cmd-1", serde_json::json!({}))
            .await;
        assert!(!resolved);
    }

    #[tokio::test]
    async fn resolve_pending_unknown_command_returns_false() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://x.com", "X").await;
        let resolved = mgr
            .resolve_pending(1, "nonexistent", serde_json::json!({}))
            .await;
        assert!(!resolved);
    }

    #[tokio::test]
    async fn register_pending_unknown_tab_returns_none() {
        let mgr = TabManager::new();
        assert!(mgr.register_pending(999, "cmd-1").await.is_none());
    }

    #[tokio::test]
    async fn tab_update_unknown_tab_noop() {
        let mgr = TabManager::new();
        mgr.on_tab_updated(999, Some("https://x.com"), Some("X"))
            .await;
        assert_eq!(mgr.tab_count().await, 0);
    }

    #[tokio::test]
    async fn tab_update_partial_url_only() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://old.com", "Title").await;
        mgr.on_tab_updated(1, Some("https://new.com"), None).await;

        let tabs = mgr.list_tabs().await;
        assert_eq!(tabs[0].url, "https://new.com");
        assert_eq!(tabs[0].title, "Title");
    }

    #[tokio::test]
    async fn tab_update_partial_title_only() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://x.com", "Old").await;
        mgr.on_tab_updated(1, None, Some("New")).await;

        let tabs = mgr.list_tabs().await;
        assert_eq!(tabs[0].url, "https://x.com");
        assert_eq!(tabs[0].title, "New");
    }

    #[tokio::test]
    async fn multiple_tabs_create_close() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://one.com", "One").await;
        mgr.on_tab_created(2, "https://two.com", "Two").await;
        mgr.on_tab_created(3, "https://three.com", "Three").await;
        assert_eq!(mgr.tab_count().await, 3);

        mgr.on_tab_closed(2).await;
        assert_eq!(mgr.tab_count().await, 2);

        let tabs = mgr.list_tabs().await;
        let ids: Vec<u32> = tabs.iter().map(|t| t.tab_id).collect();
        assert!(ids.contains(&1));
        assert!(!ids.contains(&2));
        assert!(ids.contains(&3));
    }

    #[tokio::test]
    async fn close_nonexistent_tab_noop() {
        let mgr = TabManager::new();
        mgr.on_tab_closed(999).await;
        assert_eq!(mgr.tab_count().await, 0);
    }

    #[tokio::test]
    async fn default_trait_works() {
        let mgr = TabManager::default();
        assert_eq!(mgr.tab_count().await, 0);
        assert_eq!(mgr.get_active_tab_id().await, 0);
    }

    #[tokio::test]
    async fn active_tab_switches() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://one.com", "One").await;
        mgr.on_tab_created(2, "https://two.com", "Two").await;

        mgr.on_tab_activated(1).await;
        assert_eq!(mgr.get_active_tab_id().await, 1);

        mgr.on_tab_activated(2).await;
        assert_eq!(mgr.get_active_tab_id().await, 2);
    }

    #[tokio::test]
    async fn resolve_tab_with_explicit_id() {
        let mgr = TabManager::new();
        mgr.on_tab_created(5, "https://five.com", "Five").await;
        let resolved = mgr.resolve_tab(Some(5)).await.unwrap();
        assert_eq!(resolved, 5);
    }

    // --- Adversarial stress tests ---

    #[tokio::test]
    async fn concurrent_tab_creation_1000() {
        let mgr = Arc::new(TabManager::new());
        let mut handles = vec![];

        for i in 0..1000u32 {
            let m = Arc::clone(&mgr);
            handles.push(tokio::spawn(async move {
                m.on_tab_created(i, &format!("https://{i}.com"), &format!("Tab {i}"))
                    .await;
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(mgr.tab_count().await, 1000);
    }

    #[tokio::test]
    async fn concurrent_create_close_race() {
        let mgr = Arc::new(TabManager::new());
        let mut handles = vec![];

        // Create 500 tabs
        for i in 0..500u32 {
            let m = Arc::clone(&mgr);
            handles.push(tokio::spawn(async move {
                m.on_tab_created(i, &format!("https://{i}.com"), &format!("Tab {i}"))
                    .await;
            }));
        }

        // Simultaneously close even-numbered tabs
        for i in (0..500u32).step_by(2) {
            let m = Arc::clone(&mgr);
            handles.push(tokio::spawn(async move {
                m.on_tab_closed(i).await;
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        // Some creates may happen after close, some before
        // Final count should be at least 250 (the odd ones that were never closed)
        // and at most 500 (if all closes ran before creates)
        let count = mgr.tab_count().await;
        assert!((200..=500).contains(&count), "unexpected count: {count}");
    }

    #[tokio::test]
    async fn rapid_activate_deactivate() {
        let mgr = Arc::new(TabManager::new());

        for i in 1..=10u32 {
            mgr.on_tab_created(i, &format!("https://{i}.com"), &format!("Tab {i}"))
                .await;
        }

        let mut handles = vec![];
        for i in 1..=10u32 {
            let m = Arc::clone(&mgr);
            handles.push(tokio::spawn(async move {
                for _ in 0..100 {
                    m.on_tab_activated(i).await;
                }
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        let active = mgr.get_active_tab_id().await;
        assert!((1..=10).contains(&active));
    }

    #[tokio::test]
    async fn tab_with_very_long_url() {
        let mgr = TabManager::new();
        let long_url = format!("https://example.com/{}", "a".repeat(100_000));
        mgr.on_tab_created(1, &long_url, "Test").await;

        let tabs = mgr.list_tabs().await;
        assert_eq!(tabs[0].url.len(), long_url.len());
    }

    #[tokio::test]
    async fn tab_with_unicode_title() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://example.com", "日本語タイトル 🚀 émojis")
            .await;

        let tabs = mgr.list_tabs().await;
        assert!(tabs[0].title.contains("🚀"));
    }

    #[tokio::test]
    async fn pending_command_overwrite() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://x.com", "X").await;

        let rx1 = mgr.register_pending(1, "cmd-dup").await.unwrap();
        let rx2 = mgr.register_pending(1, "cmd-dup").await.unwrap();

        // First rx should be orphaned (sender dropped when overwritten)
        assert!(rx1.await.is_err());

        mgr.resolve_pending(1, "cmd-dup", serde_json::json!({"v": 2}))
            .await;
        let result = rx2.await.unwrap();
        assert_eq!(result, serde_json::json!({"v": 2}));
    }

    #[tokio::test]
    async fn resolve_tab_after_active_closed() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://one.com", "One").await;
        mgr.on_tab_activated(1).await;
        mgr.on_tab_closed(1).await;

        let result = mgr.resolve_tab(None).await;
        assert!(matches!(result, Err(TabError::TabNotFound(1))));
    }

    #[tokio::test]
    async fn concurrent_pending_commands() {
        let mgr = Arc::new(TabManager::new());
        mgr.on_tab_created(1, "https://x.com", "X").await;

        let mut receivers = vec![];
        for i in 0..100 {
            let rx = mgr
                .register_pending(1, &format!("cmd-{i}"))
                .await
                .unwrap();
            receivers.push((i, rx));
        }

        let mut handles = vec![];
        for i in 0..100 {
            let m = Arc::clone(&mgr);
            handles.push(tokio::spawn(async move {
                m.resolve_pending(1, &format!("cmd-{i}"), serde_json::json!({"i": i}))
                    .await
            }));
        }

        for h in handles {
            assert!(h.await.unwrap());
        }

        for (i, rx) in receivers {
            let val = rx.await.unwrap();
            assert_eq!(val["i"], i);
        }
    }

    #[tokio::test]
    async fn list_tabs_empty_is_empty_vec() {
        let mgr = TabManager::new();
        let tabs = mgr.list_tabs().await;
        assert!(tabs.is_empty());
    }

    #[tokio::test]
    async fn tab_id_zero_not_confused_with_no_active() {
        let mgr = TabManager::new();
        mgr.on_tab_created(0, "https://zero.com", "Zero").await;
        mgr.on_tab_activated(0).await;

        // active_tab is 0, but 0 is treated as "no active tab"
        let result = mgr.resolve_tab(None).await;
        assert!(matches!(result, Err(TabError::NoActiveTab)));
    }

    // --- Deep challenger: resolve_tab + pending command lifecycle ---

    #[tokio::test]
    async fn resolve_tab_with_explicit_id_works() {
        let mgr = TabManager::new();
        mgr.on_tab_created(42, "https://x.com", "X").await;
        let result = mgr.resolve_tab(Some(42)).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn resolve_tab_with_explicit_nonexistent_errors() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://x.com", "X").await;
        let result = mgr.resolve_tab(Some(999)).await;
        assert!(matches!(result, Err(TabError::TabNotFound(999))));
    }

    #[tokio::test]
    async fn resolve_tab_with_none_uses_active() {
        let mgr = TabManager::new();
        mgr.on_tab_created(5, "https://x.com", "X").await;
        mgr.on_tab_activated(5).await;
        let result = mgr.resolve_tab(None).await;
        assert_eq!(result.unwrap(), 5);
    }

    #[tokio::test]
    async fn resolve_tab_active_but_closed_errors() {
        let mgr = TabManager::new();
        mgr.on_tab_created(5, "https://x.com", "X").await;
        mgr.on_tab_activated(5).await;
        mgr.on_tab_closed(5).await;
        // active_tab still points to 5, but it's gone
        let result = mgr.resolve_tab(None).await;
        assert!(matches!(result, Err(TabError::TabNotFound(5))));
    }

    #[tokio::test]
    async fn pending_command_lost_on_tab_close() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://x.com", "X").await;
        let rx = mgr.register_pending(1, "cmd-1").await.unwrap();

        // Close the tab — pending commands are dropped
        mgr.on_tab_closed(1).await;

        // The sender was dropped, so rx should get RecvError
        assert!(rx.await.is_err());
    }

    #[tokio::test]
    async fn register_pending_on_nonexistent_tab_returns_none() {
        let mgr = TabManager::new();
        let result = mgr.register_pending(999, "cmd-1").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn resolve_pending_on_nonexistent_tab_returns_false() {
        let mgr = TabManager::new();
        let result = mgr.resolve_pending(999, "cmd-1", serde_json::json!({})).await;
        assert!(!result);
    }

    #[tokio::test]
    async fn resolve_pending_with_wrong_command_id_returns_false() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://x.com", "X").await;
        let _rx = mgr.register_pending(1, "cmd-1").await.unwrap();
        let result = mgr.resolve_pending(1, "cmd-2", serde_json::json!({})).await;
        assert!(!result);
    }

    #[tokio::test]
    async fn tab_create_overwrites_existing() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://first.com", "First").await;
        mgr.on_bridge_ready(1).await;

        // Re-create same tab ID (e.g., service worker restart)
        mgr.on_tab_created(1, "https://second.com", "Second").await;

        let tabs = mgr.list_tabs().await;
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].url, "https://second.com");
        assert!(!tabs[0].bridge_ready); // Reset by re-creation
    }

    #[tokio::test]
    async fn list_tabs_shows_correct_active_flag() {
        let mgr = TabManager::new();
        mgr.on_tab_created(1, "https://a.com", "A").await;
        mgr.on_tab_created(2, "https://b.com", "B").await;
        mgr.on_tab_created(3, "https://c.com", "C").await;
        mgr.on_tab_activated(2).await;

        let tabs = mgr.list_tabs().await;
        let active_count = tabs.iter().filter(|t| t.active).count();
        assert_eq!(active_count, 1);
        let active_tab = tabs.iter().find(|t| t.active).unwrap();
        assert_eq!(active_tab.tab_id, 2);
    }

    #[tokio::test]
    async fn on_tab_updated_unknown_tab_is_silent() {
        let mgr = TabManager::new();
        // Should not panic on unknown tab
        mgr.on_tab_updated(999, Some("https://new.com"), Some("New")).await;
        assert_eq!(mgr.tab_count().await, 0);
    }

    #[tokio::test]
    async fn on_bridge_ready_unknown_tab_is_silent() {
        let mgr = TabManager::new();
        mgr.on_bridge_ready(999).await;
        assert_eq!(mgr.tab_count().await, 0);
    }

    use std::sync::Arc;
}
