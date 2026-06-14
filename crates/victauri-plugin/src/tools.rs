use std::sync::Arc;
use tauri::{Runtime, State};
use victauri_core::{IpcCall, WindowState};

use crate::VictauriState;

#[tauri::command]
pub async fn victauri_eval_js<R: Runtime>(
    webview: tauri::WebviewWindow<R>,
    state: State<'_, Arc<VictauriState>>,
    code: String,
) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();

    state.pending_evals.lock().await.insert(id.clone(), tx);

    let inject = format!(
        r"
        (async () => {{
            try {{
                const __result = await (async () => {{ {code} }})();
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

    if let Err(e) = webview.eval(&inject) {
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
    let id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();

    state.pending_evals.lock().await.insert(id.clone(), tx);

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

    if let Err(e) = webview.eval(&inject) {
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
