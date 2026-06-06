use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncWrite;
use tokio::sync::{Mutex, oneshot};

const DISPATCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum number of concurrently in-flight commands. Bounds memory if a caller
/// floods the host or the extension stops responding (audit #12); each entry is
/// reaped on response/timeout/disconnect, so this is only a backpressure ceiling.
const MAX_PENDING: usize = 1024;

/// Manages in-flight commands sent to the Chrome extension via native messaging.
///
/// Each command gets a UUID, is written to the native messaging stdout, and
/// a oneshot receiver awaits the response from the extension.
pub struct BridgeDispatch {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<DispatchResult>>>>,
    /// Boxed trait object so the writer can be the live process stdout in
    /// production or a discarding sink in tests, without making the struct
    /// generic (which would ripple a type param into every `Arc<BridgeDispatch>`
    /// field across handlers/TabManager). `Box<dyn AsyncWrite + Send + Unpin>`
    /// is itself `AsyncWrite + Unpin`, so it plugs straight into
    /// `native_messaging::write_message`.
    writer: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
}

#[derive(Debug)]
pub struct DispatchResult {
    pub data: Option<Value>,
    pub error: Option<String>,
}

impl BridgeDispatch {
    #[must_use]
    pub fn new<W: AsyncWrite + Send + Unpin + 'static>(writer: W) -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            writer: Arc::new(Mutex::new(Box::new(writer))),
        }
    }

    /// Construct a dispatch whose frames are written to a discarding sink.
    ///
    /// Use this in tests so dispatched native-messaging frames are not emitted
    /// into `cargo test` stdout (which would corrupt the test output stream).
    #[must_use]
    pub fn new_sink() -> Self {
        Self::new(tokio::io::sink())
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
            if pending.len() >= MAX_PENDING {
                return Err(format!(
                    "too many in-flight commands ({MAX_PENDING}); extension unresponsive"
                ));
            }
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
            if let Err(e) = crate::native_messaging::write_message(&mut *writer, &msg).await {
                // The command never reached the extension, so its response will
                // never arrive — reap the pending entry now instead of leaving it
                // to occupy a MAX_PENDING slot until the 30s timeout (audit B9).
                drop(writer);
                self.cleanup_pending(&id).await;
                return Err(format!("native messaging write failed: {e}"));
            }
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
                Err(format!(
                    "timeout ({DISPATCH_TIMEOUT:?}) waiting for {method}"
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
    #[allow(dead_code)]
    pub async fn pending_count(&self) -> usize {
        self.pending.lock().await.len()
    }

    async fn cleanup_pending(&self, id: &str) {
        let mut pending = self.pending.lock().await;
        pending.remove(id);
    }

    /// Return the IDs of all currently pending commands (for testing).
    pub async fn pending_ids(&self) -> Vec<String> {
        self.pending.lock().await.keys().cloned().collect()
    }

    /// Insert a pending command directly and return the receiver (for testing).
    pub async fn register_test_pending(&self, id: &str) -> oneshot::Receiver<DispatchResult> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.to_string(), tx);
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer whose `poll_write` always fails — used to verify that a failed
    /// native-messaging send reaps its pending entry instead of leaking it.
    struct FailingWriter;

    impl AsyncWrite for FailingWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Err(std::io::Error::other("simulated stdout failure")))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn dispatch_write_failure_reaps_pending_entry() {
        let dispatch = BridgeDispatch::new(FailingWriter);

        let result = dispatch
            .dispatch(Some(1), "snapshot", serde_json::json!({}))
            .await;

        // The write failed, so dispatch returns an error...
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("write failed"));
        // ...and crucially the pending map is empty — no leaked slot (audit B9).
        assert_eq!(dispatch.pending_count().await, 0);
    }

    #[tokio::test]
    async fn on_response_resolves_pending() {
        let dispatch = BridgeDispatch::new_sink();

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
        let dispatch = BridgeDispatch::new_sink();

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
        let dispatch = BridgeDispatch::new_sink();

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
        let dispatch = BridgeDispatch::new_sink();

        dispatch
            .on_response("nonexistent", Some(serde_json::json!({})), None)
            .await;

        assert_eq!(dispatch.pending_count().await, 0);
    }

    #[tokio::test]
    async fn pending_count_tracks_insertions() {
        let dispatch = BridgeDispatch::new_sink();

        assert_eq!(dispatch.pending_count().await, 0);

        let (tx1, _rx1) = oneshot::channel();
        let (tx2, _rx2) = oneshot::channel();
        {
            let mut pending = dispatch.pending.lock().await;
            pending.insert("a".to_string(), tx1);
            pending.insert("b".to_string(), tx2);
        }
        assert_eq!(dispatch.pending_count().await, 2);

        dispatch
            .on_response("a", Some(serde_json::json!({"ok": true})), None)
            .await;
        assert_eq!(dispatch.pending_count().await, 1);
    }

    #[tokio::test]
    async fn on_response_with_null_data_and_no_error() {
        let dispatch = BridgeDispatch::new_sink();

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = dispatch.pending.lock().await;
            pending.insert("test-null".to_string(), tx);
        }

        dispatch.on_response("test-null", None, None).await;

        let result = rx.await.unwrap();
        assert!(result.data.is_none());
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn cancel_all_with_multiple_pending() {
        let dispatch = BridgeDispatch::new_sink();

        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();
        let (tx3, rx3) = oneshot::channel();
        {
            let mut pending = dispatch.pending.lock().await;
            pending.insert("a".to_string(), tx1);
            pending.insert("b".to_string(), tx2);
            pending.insert("c".to_string(), tx3);
        }

        dispatch.cancel_all().await;
        assert_eq!(dispatch.pending_count().await, 0);

        for rx in [rx1, rx2, rx3] {
            let result = rx.await.unwrap();
            assert!(result.error.is_some());
            assert!(result.error.unwrap().contains("disconnected"));
        }
    }

    #[tokio::test]
    async fn cancel_all_on_empty_is_noop() {
        let dispatch = BridgeDispatch::new_sink();
        dispatch.cancel_all().await;
        assert_eq!(dispatch.pending_count().await, 0);
    }

    // --- Adversarial stress tests ---

    #[tokio::test]
    async fn concurrent_100_pending_insertions_and_resolutions() {
        let dispatch = Arc::new(BridgeDispatch::new_sink());

        let mut receivers = vec![];
        for i in 0..100 {
            let (tx, rx) = oneshot::channel();
            {
                let mut pending = dispatch.pending.lock().await;
                pending.insert(format!("stress-{i}"), tx);
            }
            receivers.push((i, rx));
        }
        assert_eq!(dispatch.pending_count().await, 100);

        let mut handles = vec![];
        for i in 0..100 {
            let d = Arc::clone(&dispatch);
            handles.push(tokio::spawn(async move {
                d.on_response(
                    &format!("stress-{i}"),
                    Some(serde_json::json!({"idx": i})),
                    None,
                )
                .await;
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(dispatch.pending_count().await, 0);
        for (i, rx) in receivers {
            let result = rx.await.unwrap();
            assert_eq!(result.data.unwrap()["idx"], i);
        }
    }

    #[tokio::test]
    async fn resolve_after_cancel_all_is_noop() {
        let dispatch = BridgeDispatch::new_sink();

        let (tx, _rx) = oneshot::channel();
        {
            let mut pending = dispatch.pending.lock().await;
            pending.insert("doomed".to_string(), tx);
        }

        dispatch.cancel_all().await;

        // Trying to resolve after cancel should be a no-op (key already removed)
        dispatch
            .on_response("doomed", Some(serde_json::json!({"late": true})), None)
            .await;
        assert_eq!(dispatch.pending_count().await, 0);
    }

    #[tokio::test]
    async fn duplicate_id_response_only_resolves_once() {
        let dispatch = BridgeDispatch::new_sink();

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = dispatch.pending.lock().await;
            pending.insert("dup".to_string(), tx);
        }

        dispatch
            .on_response("dup", Some(serde_json::json!({"first": true})), None)
            .await;
        // Second response with same ID should be silently ignored
        dispatch
            .on_response("dup", Some(serde_json::json!({"second": true})), None)
            .await;

        let result = rx.await.unwrap();
        assert_eq!(result.data.unwrap()["first"], true);
    }

    #[tokio::test]
    async fn cancel_all_then_insert_new() {
        let dispatch = BridgeDispatch::new_sink();

        let (tx1, rx1) = oneshot::channel();
        {
            let mut pending = dispatch.pending.lock().await;
            pending.insert("before".to_string(), tx1);
        }

        dispatch.cancel_all().await;
        let result1 = rx1.await.unwrap();
        assert!(result1.error.is_some());

        // New insertions after cancel should work normally
        let (tx2, rx2) = oneshot::channel();
        {
            let mut pending = dispatch.pending.lock().await;
            pending.insert("after".to_string(), tx2);
        }
        assert_eq!(dispatch.pending_count().await, 1);

        dispatch
            .on_response("after", Some(serde_json::json!({"ok": true})), None)
            .await;
        let result2 = rx2.await.unwrap();
        assert_eq!(result2.data.unwrap()["ok"], true);
    }

    #[tokio::test]
    async fn concurrent_cancel_and_resolve_race() {
        let dispatch = Arc::new(BridgeDispatch::new_sink());

        for i in 0..50 {
            let (tx, _rx) = oneshot::channel();
            let mut pending = dispatch.pending.lock().await;
            pending.insert(format!("race-{i}"), tx);
        }

        let d1 = Arc::clone(&dispatch);
        let cancel_task = tokio::spawn(async move {
            d1.cancel_all().await;
        });

        let d2 = Arc::clone(&dispatch);
        let resolve_task = tokio::spawn(async move {
            for i in 0..50 {
                d2.on_response(&format!("race-{i}"), Some(serde_json::json!({})), None)
                    .await;
            }
        });

        cancel_task.await.unwrap();
        resolve_task.await.unwrap();

        // Regardless of ordering, pending should be empty
        assert_eq!(dispatch.pending_count().await, 0);
    }

    #[tokio::test]
    async fn on_response_with_both_data_and_error() {
        let dispatch = BridgeDispatch::new_sink();

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = dispatch.pending.lock().await;
            pending.insert("both".to_string(), tx);
        }

        dispatch
            .on_response(
                "both",
                Some(serde_json::json!({"partial": true})),
                Some("also an error".to_string()),
            )
            .await;

        let result = rx.await.unwrap();
        assert!(result.data.is_some());
        assert!(result.error.is_some());
    }

    use std::sync::Arc;
}
