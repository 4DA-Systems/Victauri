use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{Mutex, oneshot};

const DISPATCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Manages in-flight commands sent to the Chrome extension via native messaging.
///
/// Each command gets a UUID, is written to the native messaging stdout, and
/// a oneshot receiver awaits the response from the extension.
pub struct BridgeDispatch {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<DispatchResult>>>>,
    writer: Arc<Mutex<tokio::io::Stdout>>,
}

#[derive(Debug)]
pub struct DispatchResult {
    pub data: Option<Value>,
    pub error: Option<String>,
}

impl BridgeDispatch {
    #[must_use]
    pub fn new(writer: tokio::io::Stdout) -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            writer: Arc::new(Mutex::new(writer)),
        }
    }

    /// Send a command to the extension and await the response.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails, the extension disconnects,
    /// or the command times out (30s).
    pub async fn dispatch(
        &self,
        tab_id: Option<u32>,
        method: &str,
        args: Value,
    ) -> Result<Value, String> {
        let id = uuid::Uuid::new_v4().to_string();

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id.clone(), tx);
        }

        let msg = serde_json::json!({
            "id": id,
            "type": "execute",
            "tab_id": tab_id,
            "method": method,
            "args": args,
        });

        {
            let mut writer = self.writer.lock().await;
            crate::native_messaging::write_message(&mut *writer, &msg)
                .await
                .map_err(|e| format!("native messaging write failed: {e}"))?;
        }

        match tokio::time::timeout(DISPATCH_TIMEOUT, rx).await {
            Ok(Ok(result)) => {
                if let Some(err) = result.error {
                    Err(err)
                } else {
                    Ok(result.data.unwrap_or(Value::Null))
                }
            }
            Ok(Err(_)) => {
                self.cleanup_pending(&id).await;
                Err("extension disconnected while waiting for response".to_string())
            }
            Err(_) => {
                self.cleanup_pending(&id).await;
                Err(format!("timeout ({DISPATCH_TIMEOUT:?}) waiting for {method}"))
            }
        }
    }

    /// Send a CDP command to the extension.
    ///
    /// # Errors
    ///
    /// Returns an error on write failure, disconnect, or timeout.
    pub async fn dispatch_cdp(
        &self,
        tab_id: u32,
        domain_method: &str,
        params: Option<Value>,
    ) -> Result<Value, String> {
        let id = uuid::Uuid::new_v4().to_string();

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id.clone(), tx);
        }

        let msg = serde_json::json!({
            "id": id,
            "type": "cdp",
            "tab_id": tab_id,
            "domain_method": domain_method,
            "params": params.unwrap_or(Value::Null),
        });

        {
            let mut writer = self.writer.lock().await;
            crate::native_messaging::write_message(&mut *writer, &msg)
                .await
                .map_err(|e| format!("native messaging write failed: {e}"))?;
        }

        match tokio::time::timeout(DISPATCH_TIMEOUT, rx).await {
            Ok(Ok(result)) => {
                if let Some(err) = result.error {
                    Err(err)
                } else {
                    Ok(result.data.unwrap_or(Value::Null))
                }
            }
            Ok(Err(_)) => {
                self.cleanup_pending(&id).await;
                Err("extension disconnected during CDP call".to_string())
            }
            Err(_) => {
                self.cleanup_pending(&id).await;
                Err(format!(
                    "timeout ({DISPATCH_TIMEOUT:?}) waiting for CDP {domain_method}"
                ))
            }
        }
    }

    /// Called by the native messaging read loop when a response arrives.
    pub async fn on_response(&self, id: &str, data: Option<Value>, error: Option<String>) {
        let mut pending = self.pending.lock().await;
        if let Some(tx) = pending.remove(id) {
            let _ = tx.send(DispatchResult { data, error });
        }
    }

    /// Drop all pending commands (e.g. on disconnect).
    pub async fn cancel_all(&self) {
        let mut pending = self.pending.lock().await;
        for (_, tx) in pending.drain() {
            let _ = tx.send(DispatchResult {
                data: None,
                error: Some("extension disconnected".to_string()),
            });
        }
    }

    #[must_use]
    pub async fn pending_count(&self) -> usize {
        self.pending.lock().await.len()
    }

    async fn cleanup_pending(&self, id: &str) {
        let mut pending = self.pending.lock().await;
        pending.remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn on_response_resolves_pending() {
        let stdout = tokio::io::stdout();
        let dispatch = BridgeDispatch::new(stdout);

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = dispatch.pending.lock().await;
            pending.insert("test-123".to_string(), tx);
        }

        dispatch
            .on_response("test-123", Some(serde_json::json!({"ok": true})), None)
            .await;

        let result = rx.await.unwrap();
        assert!(result.error.is_none());
        assert_eq!(result.data.unwrap(), serde_json::json!({"ok": true}));
    }

    #[tokio::test]
    async fn on_response_with_error() {
        let stdout = tokio::io::stdout();
        let dispatch = BridgeDispatch::new(stdout);

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = dispatch.pending.lock().await;
            pending.insert("test-456".to_string(), tx);
        }

        dispatch
            .on_response("test-456", None, Some("bridge timeout".to_string()))
            .await;

        let result = rx.await.unwrap();
        assert_eq!(result.error.unwrap(), "bridge timeout");
    }

    #[tokio::test]
    async fn cancel_all_resolves_pending() {
        let stdout = tokio::io::stdout();
        let dispatch = BridgeDispatch::new(stdout);

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = dispatch.pending.lock().await;
            pending.insert("test-789".to_string(), tx);
        }

        dispatch.cancel_all().await;

        let result = rx.await.unwrap();
        assert!(result.error.is_some());
        assert_eq!(dispatch.pending_count().await, 0);
    }

    #[tokio::test]
    async fn unknown_response_id_ignored() {
        let stdout = tokio::io::stdout();
        let dispatch = BridgeDispatch::new(stdout);

        dispatch
            .on_response("nonexistent", Some(serde_json::json!({})), None)
            .await;

        assert_eq!(dispatch.pending_count().await, 0);
    }
}
