use std::sync::Arc;
use tauri::{Manager, Runtime, State};
use victauri_core::{IpcCall, WindowState};

use crate::VictauriState;

/// Id prefix for evals started by page JS through these Tauri commands (vs. by an agent). The
/// full id is `page:<uuid>:<window label>` (the uuid is fixed-length, so any label parses back).
pub const PAGE_EVAL_PREFIX: &str = "page:";
/// Pending-eval slots page-originated evals may hold at once, across all windows. They share
/// the pending map with the agent's (MCP) evals; without their own small budget, page script
/// could park never-resolving evals in every slot and starve the agent with "too many
/// concurrent evals".
pub const MAX_PAGE_PENDING_EVALS: usize = 25;
/// Page-originated eval slots ONE window may hold, so one window cannot starve the others.
pub const MAX_PAGE_PENDING_EVALS_PER_WINDOW: usize = 10;

/// Page-callable window queries (`victauri_list_windows` / `victauri_get_window_state`) allowed
/// in flight at once, across all windows. Each is a main-thread round trip serialized with every
/// other one (the agent's included); without a budget page script could queue hundreds and
/// starve the agent. Beyond it a query is refused at once, like a page eval over its budget.
pub const MAX_PAGE_WINDOW_QUERIES: usize = 4;
pub(crate) static PAGE_WINDOW_QUERY_SLOTS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(MAX_PAGE_WINDOW_QUERIES);

/// Run page-originated window query `f` within [`MAX_PAGE_WINDOW_QUERIES`].
pub(crate) fn page_window_query<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    let _slot = PAGE_WINDOW_QUERY_SLOTS.try_acquire().map_err(|_| {
        format!(
            "too many concurrent page-originated window queries (limit \
             {MAX_PAGE_WINDOW_QUERIES}); retry when one has finished"
        )
    })?;
    Ok(f())
}

/// The window a page-originated eval id belongs to.
fn page_eval_window(id: &str) -> Option<&str> {
    let rest = id.strip_prefix(PAGE_EVAL_PREFIX)?;
    rest.get(36..)?.strip_prefix(':')
}

/// Reserve a pending-eval slot for a page-originated eval from window `label`, within the
/// per-window budget, the page budget and the global ceiling. The slot is released when the
/// returned guard drops.
pub async fn reserve_page_eval(
    state: &VictauriState,
    label: &str,
    tx: tokio::sync::oneshot::Sender<String>,
) -> Result<crate::PendingSlot, String> {
    let id = format!("{PAGE_EVAL_PREFIX}{}:{label}", uuid::Uuid::new_v4());
    let mut pending = state.pending_evals.lock().await;
    let mut page_pending = 0;
    let mut window_pending = 0;
    for key in pending.keys().filter(|k| k.starts_with(PAGE_EVAL_PREFIX)) {
        page_pending += 1;
        if page_eval_window(key) == Some(label) {
            window_pending += 1;
        }
    }
    if window_pending >= MAX_PAGE_PENDING_EVALS_PER_WINDOW {
        return Err(format!(
            "too many concurrent page-originated evals from window '{label}' \
             (limit {MAX_PAGE_PENDING_EVALS_PER_WINDOW})"
        ));
    }
    if page_pending >= MAX_PAGE_PENDING_EVALS {
        return Err(format!(
            "too many concurrent page-originated evals (limit {MAX_PAGE_PENDING_EVALS})"
        ));
    }
    if pending.len() >= crate::mcp::MAX_PENDING_EVALS {
        return Err(format!(
            "too many concurrent evals (limit {})",
            crate::mcp::MAX_PENDING_EVALS
        ));
    }
    Ok(crate::PendingSlot::insert(
        &state.pending_evals,
        &mut pending,
        id,
        tx,
    ))
}

#[tauri::command]
pub async fn victauri_eval_js<R: Runtime>(
    webview: tauri::WebviewWindow<R>,
    state: State<'_, Arc<VictauriState>>,
    code: String,
) -> Result<String, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    // Released on every exit, including this future being dropped mid-wait.
    let slot = reserve_page_eval(&state, webview.label(), tx).await?;
    let id = serde_json::to_string(slot.id()).map_err(|e| e.to_string())?;

    let inject = format!(
        r"
        (async () => {{
            try {{
                const __result = await (async () => {{ {code}
 }})();
                await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                    id: {id},
                    result: JSON.stringify(__result)
                }});
            }} catch (e) {{
                await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                    id: {id},
                    result: JSON.stringify({{ __error: e.message }})
                }});
            }}
        }})();
        "
    );

    // Through the bridge, i.e. on the main thread and serialized with every other
    // round trip — not a direct `webview.eval` from this tokio worker thread.
    if let Err(e) = crate::bridge::WebviewBridge::eval_webview(
        webview.app_handle(),
        Some(webview.label()),
        &inject,
    ) {
        return Err(format!("eval failed: {e}"));
    }

    match tokio::time::timeout(state.eval_timeout, rx).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(_)) => Err("eval callback channel closed".to_string()),
        Err(_) => Err(format!(
            "eval timed out after {}s",
            state.eval_timeout.as_secs()
        )),
    }
}

#[tauri::command]
pub async fn victauri_eval_callback<R: Runtime>(
    webview: tauri::Webview<R>,
    state: State<'_, Arc<VictauriState>>,
    id: String,
    result: String,
) -> Result<(), String> {
    if id == "__victauri_bridge_ready__" {
        state
            .bridge_ready
            .store(true, std::sync::atomic::Ordering::Release);
        state.bridge_notify.notify_waiters();
        // A (re)initialized bridge means this window loaded a new page: any eval still pending
        // in its previous page can never answer. The signal carries the new page's nonce; the
        // label comes from Tauri (the calling webview), so page script cannot touch another
        // window's record. (A forged signal from the same page is weeded out by the eval,
        // which asks the page for its current nonce before treating a signal as a reload.)
        let nonce = (!result.is_empty()).then_some(result.as_str());
        state.page_loads.record_load(webview.label(), nonce);
        return Ok(());
    }
    if let Some(tx) = state.pending_evals.lock().await.remove(&id) {
        let _ = tx.send(result);
    }
    Ok(())
}

#[tauri::command]
pub async fn victauri_dom_snapshot<R: Runtime>(
    webview: tauri::WebviewWindow<R>,
    state: State<'_, Arc<VictauriState>>,
) -> Result<String, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    // Released on every exit, including this future being dropped mid-wait.
    let slot = reserve_page_eval(&state, webview.label(), tx).await?;
    let id = serde_json::to_string(slot.id()).map_err(|e| e.to_string())?;

    let inject = format!(
        r"
        (async () => {{
            try {{
                const snapshot = window.__VICTAURI__?.snapshot();
                await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                    id: {id},
                    result: JSON.stringify(snapshot)
                }});
            }} catch (e) {{
                await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                    id: {id},
                    result: JSON.stringify({{ __error: e.message }})
                }});
            }}
        }})();
        "
    );

    // Through the bridge, i.e. on the main thread and serialized with every other
    // round trip — not a direct `webview.eval` from this tokio worker thread.
    if let Err(e) = crate::bridge::WebviewBridge::eval_webview(
        webview.app_handle(),
        Some(webview.label()),
        &inject,
    ) {
        return Err(format!("snapshot eval failed: {e}"));
    }

    match tokio::time::timeout(state.eval_timeout, rx).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(_)) => Err("snapshot callback channel closed".to_string()),
        Err(_) => Err(format!(
            "snapshot timed out after {}s",
            state.eval_timeout.as_secs()
        )),
    }
}

#[tauri::command]
pub async fn victauri_get_window_state<R: Runtime>(
    app: tauri::AppHandle<R>,
    label: Option<String>,
) -> Result<Vec<WindowState>, String> {
    page_window_query(|| crate::bridge::WebviewBridge::get_window_states(&app, label.as_deref()))
}

#[tauri::command]
pub async fn victauri_list_windows<R: Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<Vec<String>, String> {
    page_window_query(|| crate::bridge::WebviewBridge::list_window_labels(&app))
}

#[tauri::command]
pub async fn victauri_get_ipc_log(
    state: State<'_, Arc<VictauriState>>,
    limit: Option<usize>,
) -> Result<Vec<IpcCall>, String> {
    let mut calls = state.event_log.ipc_calls();
    if let Some(limit) = limit {
        let start = calls.len().saturating_sub(limit);
        calls = calls[start..].to_vec();
    }
    Ok(calls)
}

#[tauri::command]
pub async fn victauri_get_registry(
    state: State<'_, Arc<VictauriState>>,
    query: Option<String>,
) -> Result<serde_json::Value, String> {
    let commands = match query {
        Some(q) => state.registry.search(&q),
        None => state.registry.list(),
    };
    serde_json::to_value(commands).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn victauri_get_memory_stats() -> Result<serde_json::Value, String> {
    Ok(crate::memory::current_stats())
}

#[tauri::command]
pub async fn victauri_verify_state(
    _state: State<'_, Arc<VictauriState>>,
    frontend_state: serde_json::Value,
    backend_state: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let result = victauri_core::verify_state(frontend_state, backend_state);
    serde_json::to_value(result).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn victauri_detect_ghost_commands(
    state: State<'_, Arc<VictauriState>>,
) -> Result<serde_json::Value, String> {
    let ipc_calls = state.event_log.ipc_calls();
    let frontend_commands: Vec<String> = ipc_calls
        .iter()
        .map(|c| c.command.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let report = victauri_core::detect_ghost_commands(&frontend_commands, &state.registry);
    serde_json::to_value(report).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn victauri_check_ipc_integrity(
    state: State<'_, Arc<VictauriState>>,
    stale_threshold_ms: Option<i64>,
) -> Result<serde_json::Value, String> {
    let threshold = stale_threshold_ms.unwrap_or(5000);
    let report = victauri_core::check_ipc_integrity(&state.event_log, threshold);
    serde_json::to_value(report).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G-2: `victauri_list_windows` / `victauri_get_window_state` are callable by page script
    /// and each is a main-thread round trip serialized with every other; without a budget a
    /// page could queue hundreds and starve the agent's calls. Beyond the budget a query is
    /// refused at once, and a slot is released when its query finishes.
    #[test]
    fn page_window_queries_are_budgeted() {
        let held: Vec<_> = (0..MAX_PAGE_WINDOW_QUERIES)
            .map(|_| PAGE_WINDOW_QUERY_SLOTS.try_acquire().unwrap())
            .collect();
        let refused = page_window_query(|| 1);
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.contains("page-originated window queries")),
            "{refused:?}"
        );
        drop(held);
        assert_eq!(page_window_query(|| 7), Ok(7));
        assert_eq!(
            PAGE_WINDOW_QUERY_SLOTS.available_permits(),
            MAX_PAGE_WINDOW_QUERIES
        );
    }
}
