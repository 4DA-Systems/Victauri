use std::sync::Arc;
use tauri::{Manager, Runtime, State};
use victauri_core::{IpcCall, WindowState};

use crate::VictauriState;

/// Id prefix for evals started by page JS through these Tauri commands (vs. by an agent).
pub const PAGE_EVAL_PREFIX: &str = "page:";
/// Pending-eval slots page-originated evals may hold at once. They share the pending map with
/// the agent's (MCP) evals; without their own small budget, page script could park
/// never-resolving evals in every slot and starve the agent with "too many concurrent evals".
pub const MAX_PAGE_PENDING_EVALS: usize = 10;

/// Reserve a pending-eval slot for a page-originated eval, within both the page budget and the
/// global ceiling.
pub async fn reserve_page_eval(
    state: &VictauriState,
    id: &str,
    tx: tokio::sync::oneshot::Sender<String>,
) -> Result<(), String> {
    let mut pending = state.pending_evals.lock().await;
    let page_pending = pending
        .keys()
        .filter(|k| k.starts_with(PAGE_EVAL_PREFIX))
        .count();
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
    pending.insert(id.to_string(), tx);
    Ok(())
}

#[tauri::command]
pub async fn victauri_eval_js<R: Runtime>(
    webview: tauri::WebviewWindow<R>,
    state: State<'_, Arc<VictauriState>>,
    code: String,
) -> Result<String, String> {
    let id = format!("{PAGE_EVAL_PREFIX}{}", uuid::Uuid::new_v4());
    let (tx, rx) = tokio::sync::oneshot::channel();

    reserve_page_eval(&state, &id, tx).await?;

    let inject = format!(
        r"
        (async () => {{
            try {{
                const __result = await (async () => {{ {code}
 }})();
                await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                    id: '{id}',
                    result: JSON.stringify(__result)
                }});
            }} catch (e) {{
                await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                    id: '{id}',
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
        state.pending_evals.lock().await.remove(&id);
        return Err(format!("eval failed: {e}"));
    }

    match tokio::time::timeout(state.eval_timeout, rx).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(_)) => Err("eval callback channel closed".to_string()),
        Err(_) => {
            state.pending_evals.lock().await.remove(&id);
            Err(format!(
                "eval timed out after {}s",
                state.eval_timeout.as_secs()
            ))
        }
    }
}

#[tauri::command]
pub async fn victauri_eval_callback(
    state: State<'_, Arc<VictauriState>>,
    id: String,
    result: String,
) -> Result<(), String> {
    if id == "__victauri_bridge_ready__" {
        state
            .bridge_ready
            .store(true, std::sync::atomic::Ordering::Release);
        state.bridge_notify.notify_waiters();
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
    let id = format!("{PAGE_EVAL_PREFIX}{}", uuid::Uuid::new_v4());
    let (tx, rx) = tokio::sync::oneshot::channel();

    reserve_page_eval(&state, &id, tx).await?;

    let inject = format!(
        r"
        (async () => {{
            try {{
                const snapshot = window.__VICTAURI__?.snapshot();
                await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                    id: '{id}',
                    result: JSON.stringify(snapshot)
                }});
            }} catch (e) {{
                await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                    id: '{id}',
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
        state.pending_evals.lock().await.remove(&id);
        return Err(format!("snapshot eval failed: {e}"));
    }

    match tokio::time::timeout(state.eval_timeout, rx).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(_)) => Err("snapshot callback channel closed".to_string()),
        Err(_) => {
            state.pending_evals.lock().await.remove(&id);
            Err(format!(
                "snapshot timed out after {}s",
                state.eval_timeout.as_secs()
            ))
        }
    }
}

#[tauri::command]
pub async fn victauri_get_window_state<R: Runtime>(
    app: tauri::AppHandle<R>,
    label: Option<String>,
) -> Result<Vec<WindowState>, String> {
    Ok(crate::bridge::WebviewBridge::get_window_states(
        &app,
        label.as_deref(),
    ))
}

#[tauri::command]
pub async fn victauri_list_windows<R: Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<Vec<String>, String> {
    Ok(crate::bridge::WebviewBridge::list_window_labels(&app))
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
