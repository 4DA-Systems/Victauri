//! The Victauri **scale gauntlet**: a deliberately hostile, 4DA-scale Tauri app.
//!
//! It exists to be brutal to Victauri so the test battery in `tests/gauntlet.rs`
//! can prove every tool stays *correct and bounded* under the conditions that
//! make a demo-grade tool flaky on a real app:
//!
//!   * **Registry scale** — hundreds of registered commands (`get_registry`,
//!     `resolve_command`, ghost detection, coverage at scale).
//!   * **IPC flood** — the frontend bursts hundreds of invokes (log caps, the
//!     `ipc-log` resource, `check_ipc_integrity`).
//!   * **Large payloads** — a command that returns multiple MB (eval result cap
//!     + field trimming must bound it, not crash).
//!   * **Multi-window incl. a blind window** — a `secondary` window (granted
//!     `victauri:default`) and a `blind` window (deliberately *not* granted),
//!     proving cross-window event capture works and a blind window can't stall
//!     the others.
//!   * **Strict CSP** — `script-src 'self'` blocks page eval; `eval_js` must
//!     still work (privileged bridge), and the app's own code stays CSP-clean.
//!   * **Async fire-and-forget** — a pipeline that completes on a background
//!     task and emits an event (`wait_for` expression/event).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tauri::{Emitter, WebviewWindow};
use victauri_core::CommandInfo;
use victauri_plugin::inspectable;

/// Number of synthetic registry entries. These are registered as schema *data*
/// (no real handler) to exercise the registry/resolve/ghost/coverage tools at
/// a realistic scale without hand-writing hundreds of `generate_handler!` arms.
/// They are legitimately `registry_only` (registered, never invoked) — the
/// battery asserts they are NOT mistaken for ghosts.
const SYNTHETIC_COMMAND_COUNT: usize = 300;

/// Upper bound on `large_payload` so a hostile request can't OOM the app.
const MAX_PAYLOAD_KB: usize = 16 * 1024; // 16 MB

#[derive(Default)]
struct PipelineState {
    running: AtomicBool,
    processed: AtomicU64,
}

impl PipelineState {
    fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "running": self.running.load(Ordering::SeqCst),
            "processed": self.processed.load(Ordering::SeqCst),
        })
    }
}

#[tauri::command]
#[inspectable(
    description = "Greet by name",
    intent = "say hello",
    category = "demo",
    example = "greet world"
)]
fn greet(name: String) -> String {
    format!("Hello, {name}!")
}

#[tauri::command]
#[inspectable(
    description = "Echo the payload back unchanged (IPC round-trip integrity)",
    intent = "echo payload",
    category = "demo"
)]
fn echo(payload: serde_json::Value) -> serde_json::Value {
    payload
}

#[tauri::command]
#[inspectable(
    description = "Return a string of approximately size_kb kilobytes",
    intent = "return a large payload",
    category = "stress"
)]
fn large_payload(size_kb: usize) -> Result<String, String> {
    let kb = size_kb.min(MAX_PAYLOAD_KB);
    Ok("x".repeat(kb * 1024))
}

#[tauri::command]
#[inspectable(
    description = "Trivial marker used by the frontend IPC flood",
    intent = "record an ipc marker",
    category = "stress"
)]
fn flood_marker(seq: i64) -> i64 {
    seq
}

#[tauri::command]
#[inspectable(
    description = "Sleep for ms milliseconds, then return (timing/p95 stress)",
    intent = "run a slow command",
    category = "stress"
)]
async fn slow_command(ms: u64) -> String {
    tokio::time::sleep(Duration::from_millis(ms.min(2000))).await;
    format!("slept {ms}ms")
}

#[tauri::command]
#[inspectable(
    description = "Always returns an error (IPC integrity / error-path capture)",
    intent = "force a failure",
    category = "stress"
)]
fn fail_command() -> Result<(), String> {
    Err("intentional failure".to_string())
}

#[tauri::command]
#[inspectable(
    description = "Fire-and-forget background pipeline; emits 'pipeline-complete'",
    intent = "run the pipeline",
    category = "pipeline",
    example = "run the pipeline"
)]
fn run_pipeline(app: tauri::AppHandle, pipeline: tauri::State<'_, Arc<PipelineState>>) {
    let state = Arc::clone(&pipeline);
    if state.running.swap(true, Ordering::SeqCst) {
        return; // already running
    }
    state.processed.store(0, Ordering::SeqCst);
    tauri::async_runtime::spawn(async move {
        for _ in 0..5 {
            tokio::time::sleep(Duration::from_millis(80)).await;
            state.processed.fetch_add(1, Ordering::SeqCst);
        }
        state.running.store(false, Ordering::SeqCst);
        let _ = app.emit("pipeline-complete", state.processed.load(Ordering::SeqCst));
    });
}

#[tauri::command]
#[inspectable(
    description = "Read the background pipeline's status",
    intent = "read pipeline status",
    category = "pipeline"
)]
fn pipeline_status(pipeline: tauri::State<'_, Arc<PipelineState>>) -> serde_json::Value {
    pipeline.snapshot()
}

/// Build the full registry: the real inspectable schemas plus the synthetic
/// scale entries.
fn registry_schemas() -> Vec<CommandInfo> {
    let mut schemas = vec![
        greet__schema(),
        echo__schema(),
        large_payload__schema(),
        flood_marker__schema(),
        slow_command__schema(),
        fail_command__schema(),
        run_pipeline__schema(),
        pipeline_status__schema(),
    ];
    for i in 0..SYNTHETIC_COMMAND_COUNT {
        schemas.push(
            CommandInfo::new(format!("stress_cmd_{i:03}"))
                .with_description("synthetic registry-scale command (no handler)")
                .with_category("stress")
                .with_intent("exercise registry scale"),
        );
    }
    schemas
}

fn main() {
    let pipeline = Arc::new(PipelineState::default());
    let probe_pipeline = Arc::clone(&pipeline);

    tauri::Builder::default()
        .plugin(
            victauri_plugin::VictauriBuilder::new()
                .auth_disabled()
                .commands(&registry_schemas())
                .listen_events(&["pipeline-complete"])
                .probe("pipeline", move || probe_pipeline.snapshot())
                .build()
                .expect("victauri config is valid"),
        )
        .manage(pipeline)
        .invoke_handler(tauri::generate_handler![
            greet,
            echo,
            large_payload,
            flood_marker,
            slow_command,
            fail_command,
            run_pipeline,
            pipeline_status,
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // Secondary window — granted victauri:default (introspectable).
            WebviewWindow::builder(
                &handle,
                "secondary",
                tauri::WebviewUrl::App("secondary.html".into()),
            )
            .title("Gauntlet — Secondary")
            .inner_size(500.0, 400.0)
            .build()?;

            // Blind window — deliberately NOT in capabilities, so the Victauri
            // bridge is silently blocked here. Victauri must report it as
            // not-introspectable and must not let it stall the healthy windows.
            WebviewWindow::builder(
                &handle,
                "blind",
                tauri::WebviewUrl::App("blind.html".into()),
            )
            .title("Gauntlet — Blind")
            .inner_size(400.0, 300.0)
            .build()?;

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running gauntlet application");
}
