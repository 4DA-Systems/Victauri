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
/// in flight at once, across all windows. Beyond it a query is refused at once, like a page eval
/// over its budget.
///
/// While both are synchronous commands this budget never binds: Tauri runs a sync command on the
/// main thread, the bridge then runs the query inline (`bridge::on_main` — no round trip, no
/// dispatch lock), and the main thread runs one command at a time, so at most one slot is ever
/// held. It is kept as a guard for the path it was written for (0.9.0 round 3, when these were
/// `async` commands whose queries were main-thread round trips queued behind the agent's): should
/// they ever run off the main thread again, a page cannot queue hundreds of them.
pub const MAX_PAGE_WINDOW_QUERIES: usize = 4;
pub static PAGE_WINDOW_QUERY_SLOTS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(MAX_PAGE_WINDOW_QUERIES);

/// Run page-originated window query `f` within [`MAX_PAGE_WINDOW_QUERIES`].
pub fn page_window_query<T>(f: impl FnOnce() -> T) -> Result<T, String> {
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

/// Deliberately a SYNCHRONOUS command. Every eval, drain tick, snapshot and bridge-ready signal
/// comes back through here. Tauri runs an `async` command on a tokio worker and drops its
/// resolver — which holds a `Webview`, i.e. a Tauri handle — there; on Linux that is an
/// off-main-thread drop of a non-atomic `Rc` (see `bridge::on_main`), and at this call rate it
/// corrupted the host's heap. A sync command runs on the main thread, where its `webview`
/// argument and its resolver are created and dropped. Result delivery never blocks that thread:
/// an uncontended map is updated in place, otherwise owned data (no Tauri handle) is handed to
/// a tokio task.
#[tauri::command]
pub fn victauri_eval_callback<R: Runtime>(
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
    deliver_eval_result(Arc::clone(state.inner()), id, result);
    Ok(())
}

/// Hand an eval result to the caller waiting on `id` without blocking the calling (main)
/// thread: in place when the pending map is free, else from a tokio task holding only owned
/// data.
fn deliver_eval_result(state: Arc<VictauriState>, id: String, result: String) {
    if let Ok(mut pending) = state.pending_evals.try_lock() {
        if let Some(tx) = pending.remove(&id) {
            let _ = tx.send(result);
        }
        return;
    }
    tauri::async_runtime::spawn(async move {
        if let Some(tx) = state.pending_evals.lock().await.remove(&id) {
            let _ = tx.send(result);
        }
    });
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

// The page-callable commands below are SYNCHRONOUS on purpose (none of them waits for anything):
// Tauri runs an `async` command on a tokio worker and creates and drops its arguments and its
// resolver there, and every Tauri handle among them carries a non-atomic `Rc` on Linux (see
// `bridge::on_main`). As sync commands they run on the main thread, where those handles belong.
// `victauri_eval_js` / `victauri_dom_snapshot` must stay `async` — they wait for the page's
// callback — so a page that calls them still takes Tauri's async-command path.
#[tauri::command]
pub fn victauri_get_window_state<R: Runtime>(
    app: tauri::AppHandle<R>,
    label: Option<String>,
) -> Result<Vec<WindowState>, String> {
    page_window_query(|| crate::bridge::WebviewBridge::get_window_states(&app, label.as_deref()))
}

#[tauri::command]
pub fn victauri_list_windows<R: Runtime>(app: tauri::AppHandle<R>) -> Result<Vec<String>, String> {
    page_window_query(|| crate::bridge::WebviewBridge::list_window_labels(&app))
}

#[tauri::command]
pub fn victauri_get_ipc_log(
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
pub fn victauri_get_registry(
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
pub fn victauri_get_memory_stats() -> Result<serde_json::Value, String> {
    Ok(crate::memory::current_stats())
}

#[tauri::command]
pub fn victauri_verify_state(
    _state: State<'_, Arc<VictauriState>>,
    frontend_state: serde_json::Value,
    backend_state: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let result = victauri_core::verify_state(frontend_state, backend_state);
    serde_json::to_value(result).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn victauri_detect_ghost_commands(
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
pub fn victauri_check_ipc_integrity(
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

    /// G-2: `victauri_list_windows` / `victauri_get_window_state` are callable by page script.
    /// Beyond the budget a query is refused at once, and a slot is released when its query
    /// finishes. (As sync commands they cannot contend for it today — see
    /// [`MAX_PAGE_WINDOW_QUERIES`]; this pins the guard itself.)
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

    /// Tauri runs an `async` command on a tokio worker and extracts/drops its arguments and its
    /// resolver (which holds a `Webview`) there; on Linux every Tauri handle carries a non-atomic
    /// `Rc`, and doing that for the eval callback on every eval corrupted the host heap (0.9.0).
    /// Only the two commands that genuinely wait for the page may be `async`. A new `async`
    /// command here must first be shown not to touch Tauri handles off the main thread.
    #[test]
    fn only_the_commands_that_wait_for_the_page_are_async() {
        let source = include_str!("tools.rs");
        let async_commands: Vec<&str> = source
            .lines()
            .filter_map(|l| l.trim_start().strip_prefix("pub async fn victauri_"))
            .map(|rest| rest.split(['<', '(']).next().unwrap_or(rest))
            .collect();
        assert_eq!(
            async_commands,
            vec!["eval_js", "dom_snapshot"],
            "only victauri_eval_js / victauri_dom_snapshot may be async commands"
        );
        assert!(
            source.contains("pub fn victauri_eval_callback<R: Runtime>("),
            "victauri_eval_callback must stay a synchronous command"
        );
    }

    /// Every `.rs` file under this crate's `src/`, as `(path, source)`.
    fn crate_sources() -> Vec<(std::path::PathBuf, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, String)>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let source = std::fs::read_to_string(&path).unwrap();
                    out.push((path, source));
                }
            }
        }
        let mut out = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut out,
        );
        out
    }

    /// The same rule from the other side: `#[tauri::command]` makes a `fn` an async
    /// command without the `async` keyword, and a command defined outside this file would escape
    /// the check above. So: no `#[tauri::command(…)]` attribute anywhere in the crate may mention
    /// `async`, and every command the plugin registers is sync except the two page waiters.
    #[test]
    fn every_registered_command_is_sync_except_the_page_waiters() {
        let sources = crate_sources();
        for (path, source) in &sources {
            for line in source.lines() {
                let attr: String = line.chars().filter(|c| !c.is_whitespace()).collect();
                if attr.starts_with("#[tauri::command(") {
                    assert!(
                        !attr.contains("async"),
                        "{}: async command attribute `{}` — see \
                         only_the_commands_that_wait_for_the_page_are_async",
                        path.display(),
                        line.trim()
                    );
                }
            }
        }

        let lib = include_str!("lib.rs");
        let start = lib
            .find("generate_handler![")
            .expect("lib.rs registers its commands with generate_handler!");
        let list = &lib[start + "generate_handler![".len()..];
        let list = &list[..list.find(']').expect("generate_handler! list is closed")];
        let registered: Vec<&str> = list
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert!(registered.len() >= 10, "parsed {registered:?}");
        for path in &registered {
            let name = path.rsplit("::").next().unwrap_or(path);
            let sync_def = format!("pub fn {name}");
            let async_def = format!("pub async fn {name}");
            let defs: Vec<bool> = sources
                .iter()
                .flat_map(|(_, s)| s.lines())
                .map(str::trim_start)
                .filter_map(|l| {
                    let is_def = |d: &str| {
                        l.strip_prefix(d)
                            .is_some_and(|rest| rest.starts_with(['<', '(']))
                    };
                    if is_def(&async_def) {
                        Some(true)
                    } else if is_def(&sync_def) {
                        Some(false)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(defs.len(), 1, "command {path}: expected one definition");
            let may_be_async = matches!(name, "victauri_eval_js" | "victauri_dom_snapshot");
            assert!(
                !defs[0] || may_be_async,
                "registered command {path} is async — only the page waiters may be"
            );
        }
    }
}
