//! The Victauri **scale gauntlet** battery.
//!
//! Drives the deliberately-hostile `gauntlet-app` (hundreds of commands, IPC
//! flood, large payloads, multi-window incl. a capability-less blind window,
//! large DOM, strict CSP) and asserts that every tool stays **correct and
//! bounded** under load — the property that separates Victauri from a tool that
//! only works in a demo. Two checks here would have caught real defects:
//!   * `ipc_log_bounded_under_flood` — the `ipc-log` truncation regression.
//!   * `multi_window_recording_sees_secondary` — the single-window-drain blindness.
//!
//! Uses the REST API for unambiguous `{result}` vs `{error}` assertions (the
//! recommended path for scripted loops). Gated by `VICTAURI_GAUNTLET=1`;
//! requires the gauntlet app running. Run:
//! ```sh
//! cargo build -p gauntlet-app --bin gauntlet-app
//! ./target/debug/gauntlet-app &           # (xvfb-run on Linux)
//! VICTAURI_GAUNTLET=1 cargo test -p gauntlet-app --test gauntlet -- --test-threads=1
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};

// ── Harness ─────────────────────────────────────────────────────────────────

fn is_gauntlet() -> bool {
    std::env::var("VICTAURI_GAUNTLET").is_ok()
}

async fn base_url() -> String {
    victauri_test::connect()
        .await
        .expect("connect to gauntlet-app (set VICTAURI_GAUNTLET=1 and launch the app)")
        .base_url()
        .to_string()
}

async fn call(base: &str, tool: &str, body: Value) -> Value {
    reqwest::Client::new()
        .post(format!("{base}/api/tools/{tool}"))
        .json(&body)
        .send()
        .await
        .expect("request failed (app crashed / not responding?)")
        .json()
        .await
        .expect("non-JSON response")
}

fn result(v: &Value) -> &Value {
    assert!(
        v.get("error").is_none(),
        "expected success, got error: {}",
        v["error"]
    );
    &v["result"]
}

async fn invoke(base: &str, command: &str, args: Value) -> Value {
    call(
        base,
        "invoke_command",
        json!({ "command": command, "args": args }),
    )
    .await
}

/// Find the first JSON array reachable from a tool result — handles both
/// `[...]` and `{key: [...]}` shapes without pinning an exact schema.
fn first_array(v: &Value) -> Vec<Value> {
    if let Some(a) = v.as_array() {
        return a.clone();
    }
    if let Some(obj) = v.as_object() {
        for val in obj.values() {
            if let Some(a) = val.as_array() {
                return a.clone();
            }
        }
    }
    vec![]
}

async fn server_alive(base: &str) -> bool {
    reqwest::Client::new()
        .get(format!("{base}/health"))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

macro_rules! gauntlet {
    ($name:ident, $base:ident, $body:block) => {
        #[tokio::test]
        async fn $name() {
            if !is_gauntlet() {
                eprintln!(
                    "SKIPPED {} — set VICTAURI_GAUNTLET=1 with the gauntlet app running",
                    stringify!($name)
                );
                return;
            }
            let $base = base_url().await;
            $body
        }
    };
}

// ── 1. Registry scale ────────────────────────────────────────────────────────

gauntlet!(registry_scale_not_truncated, base, {
    // 300 synthetic + 8 real schemas registered. get_registry must return them
    // all, not silently cap, and stay valid JSON.
    let r = call(&base, "get_registry", json!({})).await;
    let cmds = first_array(result(&r));
    assert!(
        cmds.len() >= 300,
        "registry under-reported at scale: {} commands",
        cmds.len()
    );
    let names = serde_json::to_string(&cmds).unwrap();
    assert!(
        names.contains("stress_cmd_299"),
        "missing a synthetic entry"
    );
    assert!(names.contains("run_pipeline"), "missing a real entry");
});

// ── 2. Resolve at scale ──────────────────────────────────────────────────────

gauntlet!(resolve_command_ranks_among_hundreds, base, {
    // Among 308 commands, the intent "run the pipeline" must surface the real
    // run_pipeline command in the top results — ranking can't collapse at scale.
    let r = call(
        &base,
        "resolve_command",
        json!({ "query": "run the pipeline", "limit": 5 }),
    )
    .await;
    let top = serde_json::to_string(result(&r)).unwrap();
    assert!(
        top.contains("run_pipeline"),
        "resolve_command failed to rank the right command at scale: {top}"
    );
});

// ── 3. Ghost detection — no false positives at scale ─────────────────────────

gauntlet!(ghost_detection_precise_at_scale, base, {
    // Invoke a command that has no handler and is not registered → confirmed ghost.
    let _ = invoke(&base, "ghost_command_xyz", json!({})).await;
    // Give the IPC log a moment to capture the failed call.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let r = call(&base, "detect_ghost_commands", json!({})).await;
    let report = result(&r);
    let confirmed = serde_json::to_string(&report["confirmed_ghosts"]).unwrap();
    assert!(
        confirmed.contains("ghost_command_xyz"),
        "real ghost not detected: {report}"
    );
    // The 300 registered-but-uninvoked synthetic commands must NOT be flagged as
    // ghosts — that was exactly the VIC-1 false-positive class.
    assert!(
        !confirmed.contains("stress_cmd_"),
        "false-positive ghost at scale (registry_only flagged as ghost): {confirmed}"
    );
});

// ── 4. IPC log bounded under flood (PR #18 regression guard) ─────────────────

gauntlet!(ipc_log_bounded_under_flood, base, {
    // The frontend bursts 300 invokes on load; drive 100 more to be sure.
    for i in 0..100 {
        let _ = invoke(&base, "flood_marker", json!({ "seq": i })).await;
    }
    let r = call(&base, "logs", json!({ "action": "ipc", "limit": 100 })).await;
    let entries = first_array(result(&r));
    assert!(!entries.is_empty(), "IPC log empty despite a flood");
    assert!(
        entries.len() <= 100,
        "IPC log ignored the limit under load: {} entries",
        entries.len()
    );
    // Whole response must stay bounded — never dump the full body-carrying log.
    let bytes = serde_json::to_string(result(&r)).unwrap().len();
    assert!(
        bytes < 2_000_000,
        "IPC log response unbounded under flood: {bytes} bytes"
    );
});

// ── 5. IPC integrity stays healthy under load ────────────────────────────────

gauntlet!(ipc_integrity_healthy_under_load, base, {
    let r = call(&base, "check_ipc_integrity", json!({})).await;
    let report = result(&r);
    // A flood of completed round-trips must not be reported as stuck/stale.
    assert_eq!(
        report["healthy"],
        json!(true),
        "ipc integrity unhealthy under a benign flood: {report}"
    );
});

// ── 6. Large payload is bounded, not a crash ─────────────────────────────────

gauntlet!(large_payload_is_bounded_not_a_crash, base, {
    // Request ~6 MB — past the 5 MB eval result cap. The tool must respond
    // (bounded result OR a graceful error), and the server must stay alive.
    let r = invoke(&base, "large_payload", json!({ "size_kb": 6000 })).await;
    assert!(r.is_object(), "no structured response to a large payload");
    let bytes = serde_json::to_string(&r).unwrap().len();
    assert!(
        bytes < 8_000_000,
        "large-payload response unbounded: {bytes} bytes"
    );
    assert!(
        server_alive(&base).await,
        "server died on a large payload (should cap/trim, not crash)"
    );
});

// ── 7. eval_js works under strict CSP (script-src 'self') ────────────────────

gauntlet!(eval_works_under_strict_csp, base, {
    // The page CSP blocks inline scripts and eval(); the privileged bridge eval
    // must still work. If this fails, eval_js is dead on any CSP-hardened app.
    let r = call(&base, "eval_js", json!({ "code": "return 6 * 7" })).await;
    assert_eq!(result(&r), &json!(42), "eval_js broken under strict CSP");
});

// ── 8. Multi-window: all windows enumerated; blind window flagged ────────────

gauntlet!(multi_window_enumerated_and_blind_flagged, base, {
    let r = call(&base, "window", json!({ "action": "list" })).await;
    let listed = serde_json::to_string(result(&r)).unwrap();
    for label in ["main", "secondary", "blind"] {
        assert!(
            listed.contains(label),
            "window '{label}' not listed: {listed}"
        );
    }

    // introspectability must flag the capability-less 'blind' window as blind
    // while the others are introspectable.
    let intro = call(&base, "window", json!({ "action": "introspectability" })).await;
    let report = serde_json::to_string(result(&intro)).unwrap();
    assert!(
        report.contains("blind"),
        "introspectability did not report the blind window: {report}"
    );
    // It must name the capability remedy, not just time out silently.
    assert!(
        report.contains("victauri:default") || report.contains("capabilit"),
        "introspectability gave no actionable note for the blind window: {report}"
    );
});

// ── 9. Multi-window drain sees the secondary window (PR #18 regression guard) ─

gauntlet!(multi_window_recording_sees_secondary, base, {
    // Start recording, then generate a UNIQUELY identifiable console event in the
    // SECONDARY window only. A single-window (default-only) drain would never see
    // it; the per-window drain must capture it — even with the blind window
    // present (which must not stall the drain).
    let _ = call(&base, "recording", json!({ "action": "start" })).await;

    let marker = "gauntlet-secondary-marker-9173";

    // Emit the marker in the secondary window a few times across several ~1s drain
    // cycles, so the assertion is robust to drain-cycle phase (not a single fixed
    // sleep). The drained events land in the recording's stop export.
    for _ in 0..4 {
        let r = call(
            &base,
            "eval_js",
            json!({ "code": format!("console.log('{marker}'); return 1"), "webview_label": "secondary" }),
        )
        .await;
        assert_eq!(
            result(&r),
            &json!(1),
            "could not eval into the secondary window"
        );
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
    }
    // Let the final drain cycle catch up before stopping.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    let stop = call(&base, "recording", json!({ "action": "stop" })).await;
    let session = serde_json::to_string(result(&stop)).unwrap();
    assert!(
        session.contains(marker),
        "recorder was BLIND to the secondary window (single-window drain regression). \
         A console event fired only in 'secondary' did not reach the recording's stop export."
    );
});

// ── 10. Large DOM: diagnostics + find_elements stay correct and bounded ──────

gauntlet!(large_dom_handled, base, {
    // 4000-cell grid. find_elements must locate cells; the response stays bounded.
    let r = call(&base, "find_elements", json!({ "css": ".cell" })).await;
    let cells = first_array(result(&r));
    assert!(
        !cells.is_empty(),
        "find_elements found nothing in a large DOM"
    );
    let bytes = serde_json::to_string(result(&r)).unwrap().len();
    assert!(
        bytes < 5_000_000,
        "find_elements response unbounded: {bytes} bytes"
    );

    // get_diagnostics should surface the large DOM rather than choke on it.
    let d = call(&base, "get_diagnostics", json!({})).await;
    assert!(
        d.get("error").is_none(),
        "get_diagnostics failed on a large DOM: {d}"
    );
});

// ── 11. Async fire-and-forget via wait_for event ─────────────────────────────

gauntlet!(async_pipeline_wait_for_event, base, {
    // Fire-and-forget pipeline returns instantly; wait_for the completion event.
    let _ = invoke(&base, "run_pipeline", json!({})).await;
    let r = call(
        &base,
        "wait_for",
        json!({ "condition": "event", "value": "pipeline-complete", "timeout_ms": 8000, "since_ms": 3000 }),
    )
    .await;
    assert!(
        r.get("error").is_none(),
        "wait_for event timed out on a completing pipeline: {r}"
    );
});

// ── 12. The server survives the whole gauntlet ───────────────────────────────

gauntlet!(server_survives_the_gauntlet, base, {
    // Hammer a mix of stress commands, then confirm the host is still healthy and
    // timing aggregation stayed bounded (no unbounded sample growth / blow-up).
    for i in 0..50 {
        let _ = invoke(&base, "flood_marker", json!({ "seq": i })).await;
        let _ = invoke(&base, "slow_command", json!({ "ms": 5 })).await;
    }
    let _ = invoke(&base, "fail_command", json!({})).await; // error path

    let timings = call(&base, "introspect", json!({ "action": "command_timings" })).await;
    assert!(
        timings.get("error").is_none(),
        "command_timings failed after load: {timings}"
    );
    assert!(
        server_alive(&base).await,
        "server did not survive the gauntlet"
    );
});
