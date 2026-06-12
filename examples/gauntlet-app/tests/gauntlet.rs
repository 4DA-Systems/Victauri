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

/// Display-dependent tests (screenshot, filmstrip capture, trusted input) need a
/// real window server + Screen-Recording/Accessibility grants that headless CI
/// (xvfb) can't provide. They run on the macOS validation box and local Windows
/// (set `VICTAURI_GAUNTLET_DISPLAY=1`), and skip on headless Linux CI.
fn has_display() -> bool {
    std::env::var("VICTAURI_GAUNTLET_DISPLAY").is_ok()
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
    if entries.is_empty() {
        // DIAGNOSTIC: Victauri derives the IPC log from intercepting `fetch()` to
        // http://ipc.localhost/. If the log is empty after a real flood, this
        // platform's Tauri IPC isn't going through page-level fetch (suspected on
        // WebKitGTK). Dump what the network interceptor DID capture so CI reveals
        // the actual transport/URL scheme instead of a bare "empty" failure.
        let net = call(
            &base,
            "eval_js",
            json!({ "code": "return (window.__VICTAURI__.getNetworkLog ? window.__VICTAURI__.getNetworkLog(null, 20) : []).map(function(n){return n.url;})" }),
        )
        .await;
        panic!(
            "IPC log empty despite a flood — fetch-based IPC capture may not work on this \
             platform's webview. Network URLs actually captured: {net}"
        );
    }
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
    // A console event fired ONLY in the secondary window must reach the recording.
    // A single-window (default-only) drain would never see it; the per-window drain
    // must capture it — even with the blind window present (which must not stall it).
    //
    // The drain is a ~1s background poll, so capture is inherently timing-sensitive.
    // Run up to 3 full record→emit→drain→stop cycles with a UNIQUE marker each time;
    // success on any cycle proves the multi-window drain works. Only a genuine
    // regression (the secondary window NEVER captured across ~18s) fails the test.
    let mut captured = false;
    let mut last_session = String::new();
    for attempt in 0..3 {
        let marker = format!("gauntlet-secondary-marker-{attempt}-{}", 9173 + attempt);
        let _ = call(&base, "recording", json!({ "action": "start" })).await;

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
        last_session = serde_json::to_string(result(&stop)).unwrap();
        if last_session.contains(&marker) {
            captured = true;
            break;
        }
    }
    assert!(
        captured,
        "recorder was BLIND to the secondary window across 3 cycles (~18s) — the per-window \
         drain regressed. A console event fired only in 'secondary' never reached a recording. \
         Last stop export: {}",
        &last_session[..last_session.len().min(400)]
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

// ── 13. query_db reads a real seeded database (backend differentiator) ────────

gauntlet!(query_db_reads_seeded_database, base, {
    // The app seeds a `widgets` table (3 rows) in its app-data dir — a default
    // query_db search root. Read it, and confirm read-only enforcement blocks a write.
    let r = call(
        &base,
        "query_db",
        json!({ "query": "SELECT id, name, qty FROM widgets ORDER BY id" }),
    )
    .await;
    let body = serde_json::to_string(result(&r)).unwrap();
    assert!(
        body.contains("alpha") && body.contains("gamma"),
        "query_db did not read the seeded widgets table: {body}"
    );

    // A write must be rejected (read-only by design).
    let w = call(&base, "query_db", json!({ "query": "DELETE FROM widgets" })).await;
    assert!(
        w.get("error").is_some(),
        "query_db allowed a write — read-only enforcement broken: {w}"
    );
});

// ── 14. Performance metrics report engine capabilities (cross-engine) ─────────

gauntlet!(perf_metrics_report_engine_capabilities, base, {
    // get_performance must always return an engine-capability block + DOM stats.
    // JS heap / long tasks / paint are Chromium-only: on WebKit (macOS/Linux) they
    // must be marked `unavailable`, NOT silently missing — same silent-blindness
    // class as the IPC scheme bug.
    let r = call(&base, "inspect", json!({ "action": "get_performance" })).await;
    let perf = result(&r);
    assert!(
        perf["engine"].is_object(),
        "perf result missing engine capability block: {perf}"
    );
    assert!(
        perf["dom"]["elements"].as_u64().is_some(),
        "engine-agnostic DOM stats missing from perf: {perf}"
    );
    let heap = &perf["js_heap"];
    assert!(
        heap["used_mb"].is_number() || heap["unavailable"] == json!(true),
        "js_heap neither present nor marked unavailable (silent blindness): {perf}"
    );
});

// ── 15. Fault injection applies to an agent-driven command ───────────────────

gauntlet!(fault_injection_applies, base, {
    // Inject an Error fault on flood_marker, drive it via invoke_command, confirm
    // the fault took effect (the call now errors), then clear it.
    let _ = call(&base, "fault", json!({ "action": "clear_all" })).await;
    let inj = call(
        &base,
        "fault",
        json!({ "action": "inject", "command": "flood_marker", "fault_type": "error", "error_message": "gauntlet-injected-fault" }),
    )
    .await;
    assert!(inj.get("error").is_none(), "fault inject failed: {inj}");

    let faulted = invoke(&base, "flood_marker", json!({ "seq": 1 })).await;
    let txt = serde_json::to_string(&faulted).unwrap();
    assert!(
        txt.contains("gauntlet-injected-fault") || faulted.get("error").is_some(),
        "injected fault did not take effect: {txt}"
    );

    let _ = call(&base, "fault", json!({ "action": "clear_all" })).await;
    let ok = invoke(&base, "flood_marker", json!({ "seq": 2 })).await;
    assert!(
        ok.get("error").is_none(),
        "command still faulted after clear_all: {ok}"
    );
});

// ── 16. introspect action sweep (backend introspection breadth) ──────────────

gauntlet!(introspect_action_sweep, base, {
    // Each backend-introspection action must return a structured result (not an
    // error) at 4DA scale. These are direct-AppHandle reads — the genuinely
    // non-webview part of the "full-stack" pitch.
    for action in [
        "coverage",
        "db_health",
        "plugin_state",
        "processes",
        "capabilities",
        "startup_timing",
        "plugin_tasks",
        "event_bus",
    ] {
        let r = call(&base, "introspect", json!({ "action": action })).await;
        assert!(
            r.get("error").is_none(),
            "introspect {action} errored at scale: {r}"
        );
    }
});

// ── 17. app_state probe is readable (no IPC round-trip) ──────────────────────

gauntlet!(app_state_probe_readable, base, {
    // The app registers a `pipeline` probe. Reading it returns the live backend
    // snapshot directly (no webview, no IPC) — a CDP-impossible read.
    let r = call(&base, "app_state", json!({ "probe": "pipeline" })).await;
    let body = serde_json::to_string(result(&r)).unwrap();
    assert!(
        body.contains("running") && body.contains("processed"),
        "app_state pipeline probe missing expected fields: {body}"
    );
});

// ── 18. animation: list + scrub the miscalibrated sweep (geometry, headless-ok) ─

gauntlet!(animation_lists_and_scrubs_the_sweep, base, {
    // Trigger the broken sweep, then read it via the animation engine. `list` and
    // geometry-only `scrub` are pure WAAPI (no native capture), so they run
    // headless. Validates motion introspection cross-engine.
    let trigger = "var t=document.getElementById('sweep-toast'); t.classList.remove('run'); void t.offsetWidth; t.classList.add('run'); return 1";

    let _ = call(&base, "eval_js", json!({ "code": trigger })).await;
    let list = call(&base, "animation", json!({ "action": "list" })).await;
    let list_txt = serde_json::to_string(result(&list)).unwrap();
    assert!(
        list_txt.contains("sweepBroken") || list_txt.contains("translateX"),
        "animation list did not see the running sweep: {list_txt}"
    );

    let _ = call(&base, "eval_js", json!({ "code": trigger })).await;
    let scrub = call(
        &base,
        "animation",
        json!({ "action": "scrub", "selector": "#sweep-toast", "points": 6, "capture": false }),
    )
    .await;
    assert!(
        scrub.get("error").is_none(),
        "animation scrub (geometry) failed: {scrub}"
    );
});

// ── 19-21. DISPLAY-GATED: screenshot + filmstrip + trusted input ─────────────
// These need a real window server (Screen Recording on macOS); they run on the
// macOS validation box and local Windows (VICTAURI_GAUNTLET_DISPLAY=1) and skip
// on headless Linux CI.

gauntlet!(display_screenshot_is_valid_png, base, {
    if !has_display() {
        eprintln!(
            "SKIPPED display_screenshot_is_valid_png — set VICTAURI_GAUNTLET_DISPLAY=1 on a machine with a real display"
        );
        return;
    }
    let r = call(&base, "screenshot", json!({})).await;
    let body = serde_json::to_string(result(&r)).unwrap();
    assert!(
        body.contains("iVBORw0KGgo"),
        "screenshot did not return a valid PNG (no PNG magic): {}",
        &body[..body.len().min(120)]
    );
    assert!(
        body.len() > 2000,
        "screenshot PNG implausibly small ({} chars) — likely a blank capture",
        body.len()
    );
});

gauntlet!(display_animation_filmstrip_captures, base, {
    if !has_display() {
        eprintln!(
            "SKIPPED display_animation_filmstrip_captures — needs VICTAURI_GAUNTLET_DISPLAY=1"
        );
        return;
    }
    let trigger = "var t=document.getElementById('sweep-toast'); t.classList.remove('run'); void t.offsetWidth; t.classList.add('run'); return 1";
    let _ = call(&base, "eval_js", json!({ "code": trigger })).await;
    // scrub WITH capture exercises the platform-divergent raw window-capture +
    // filmstrip compositor (the macOS CGWindowList path nothing else in CI hits).
    let r = call(
        &base,
        "animation",
        json!({ "action": "scrub", "selector": "#sweep-toast", "points": 4, "capture": true }),
    )
    .await;
    let body = serde_json::to_string(result(&r)).unwrap();
    assert!(
        body.contains("iVBORw0KGgo"),
        "animation filmstrip capture produced no PNG: {}",
        &body[..body.len().min(160)]
    );
});

gauntlet!(display_trusted_input, base, {
    if !has_display() {
        eprintln!("SKIPPED display_trusted_input — needs VICTAURI_GAUNTLET_DISPLAY=1");
        return;
    }
    // trusted:true uses real OS input on Windows (SendInput); on macOS/Linux it's a
    // stub that must fall back cleanly (clear error, not a crash). Either way the
    // call returns a structured response and leaves the server alive.
    let r = call(
        &base,
        "input",
        json!({ "action": "type_text", "test_id": "trusted-target", "text": "vic", "trusted": true }),
    )
    .await;
    assert!(
        r.is_object(),
        "trusted input gave no structured response: {r}"
    );
    assert!(server_alive(&base).await, "server died on trusted input");
});
