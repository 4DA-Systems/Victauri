//! IPC capture completeness across Tauri's two IPC transports (jsdom via Node.js).
//!
//! Tauri 2 sends IPC over a custom-protocol `fetch()` on desktop, but switches a page to
//! `window.ipc.postMessage` for good after ONE custom-protocol failure, and always uses
//! postMessage on Android (tauri `scripts/ipc-protocol.js`). The bridge used to see only the
//! fetch path, so on the fallback its IPC log — and ghost detection, `command_catalog`,
//! coverage and timings built on it — went silently empty.
//!
//! Each case loads `tests/bridge_tests/tauri_ipc_fixture.js` (Tauri 2.11.5's real `core.js` +
//! `ipc-protocol.js` with an emulated backend) BEFORE the bridge, as Tauri's own init scripts
//! run before a plugin's. Same harness as `bridge_tests.rs`; set `VICTAURI_REQUIRE_JSDOM=1`
//! (CI sets `CI`) so a missing jsdom fails instead of skipping.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use victauri_plugin::js_bridge::{BridgeCapacities, agent_key, init_script};

#[derive(Serialize)]
struct TestDef {
    bridge_script: String,
    setup_html: String,
    tests: Vec<TestCase>,
}

#[derive(Serialize)]
struct TestCase {
    name: String,
    code: String,
    pre_bridge_js: String,
    node_web_api: bool,
}

#[derive(Deserialize, Debug)]
struct TestResult {
    name: String,
    passed: bool,
    result: Option<serde_json::Value>,
    error: Option<String>,
}

fn runner_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("bridge_tests")
}

fn run(cases: Vec<(&str, &str, &str)>) -> Option<Vec<TestResult>> {
    if !runner_dir().join("node_modules").join("jsdom").exists() {
        assert!(
            std::env::var_os("CI").is_none()
                && std::env::var_os("VICTAURI_REQUIRE_JSDOM").is_none(),
            "jsdom is not installed, so the JS bridge tests cannot run: \
             `npm ci` in crates/victauri-plugin/tests/bridge_tests/"
        );
        eprintln!("SKIP: jsdom not installed (run `npm ci` in tests/bridge_tests/)");
        return None;
    }
    let fixture = std::fs::read_to_string(runner_dir().join("tauri_ipc_fixture.js"))
        .expect("read tauri_ipc_fixture.js");
    let def = TestDef {
        bridge_script: init_script(&BridgeCapacities::default()),
        setup_html: "<html lang=\"en\"><head><title>t</title></head><body></body></html>"
            .to_string(),
        tests: cases
            .into_iter()
            .map(|(name, config, code)| TestCase {
                name: name.to_string(),
                code: code.to_string(),
                pre_bridge_js: format!("window.__vtestTauriConfig = {config};\n{fixture}"),
                node_web_api: true,
            })
            .collect(),
    };
    let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
    let json = serde_json::to_string(&def)
        .expect("serialize test def")
        .replace("__AGENT_KEY__", agent_key());
    tmp.write_all(json.as_bytes()).expect("write test def");
    tmp.flush().expect("flush temp file");
    let output = Command::new("node")
        .arg(runner_dir().join("run_tests.js"))
        .arg(tmp.path())
        .output()
        .expect("failed to run node");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stdout
        .lines()
        .find(|l| l.starts_with("VICTAURI_RESULTS:"))
        .unwrap_or_else(|| panic!("No VICTAURI_RESULTS line.\nstdout: {stdout}\nstderr: {stderr}"));
    Some(
        serde_json::from_str(&line["VICTAURI_RESULTS:".len()..])
            .unwrap_or_else(|e| panic!("bad results JSON: {e}\nraw: {line}")),
    )
}

/// Run one case and return its result value (panicking with the JS error on failure).
fn run_one(name: &str, config: &str, code: &str) -> Option<serde_json::Value> {
    let results = run(vec![(name, config, code)])?;
    let r = results.into_iter().next().expect("one result");
    assert!(r.passed, "{}: {:?}", r.name, r.error);
    Some(r.result.unwrap_or(serde_json::Value::Null))
}

/// Drive three commands (ok, error, ok-with-args) through Tauri's real `invoke`.
const DRIVE_THREE: &str = r"
    var I = window.__TAURI_INTERNALS__;
    var out = {};
    out.greet = await I.invoke('greet', { name: 'Ada' });
    try { await I.invoke('fail', {}); out.fail = 'resolved?'; } catch (e) { out.fail = e; }
    out.add = await I.invoke('add', { a: 1, b: 2 });
    await new Promise(function (r) { setTimeout(r, 20); });
";

#[test]
fn fetch_path_is_captured_and_reported_as_fetch() {
    let code = format!(
        "{DRIVE_THREE}
        var V = window.__VICTAURI__;
        return {{
            app: out,
            fetched: window.__vtestBackend.fetched,
            posted: window.__vtestBackend.posted.length,
            log: V.getIpcLog(),
            status: V.getIpcCaptureStatus(),
            diag: V.getDiagnostics(),
        }};"
    );
    let Some(r) = run_one("fetch path", "{}", &code) else {
        return;
    };
    assert_eq!(r["app"]["greet"], "Hello, Ada!", "{r}");
    assert_eq!(r["app"]["fail"], "boom", "{r}");
    assert_eq!(r["app"]["add"]["sum"], 3, "{r}");
    assert_eq!(r["posted"], 0, "the fetch path must not post: {r}");
    let log = r["log"].as_array().expect("log array");
    let cmds: Vec<&str> = log.iter().map(|e| e["command"].as_str().unwrap()).collect();
    assert_eq!(cmds, ["greet", "fail", "add"], "{r}");
    assert!(log.iter().all(|e| e.get("transport").is_none()), "{r}");
    assert_eq!(log[1]["status"], "error", "{r}");
    assert_eq!(r["status"]["transport"], "fetch", "{r}");
    assert_eq!(r["status"]["capture"], "full", "{r}");
    assert_eq!(r["status"]["fetch_calls"], 3, "{r}");
    assert_eq!(
        r["diag"]["info"]["ipc_capture"]["transport"], "fetch",
        "{r}"
    );
    let warn_ids: Vec<&str> = r["diag"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["id"].as_str())
        .collect();
    assert!(!warn_ids.contains(&"ipc-postmessage-fallback"), "{r}");
}

#[test]
fn postmessage_fallback_is_captured_fully_and_flagged() {
    let code = format!(
        "{DRIVE_THREE}
        var V = window.__VICTAURI__;
        return {{
            app: out,
            fetched: window.__vtestBackend.fetched.length,
            posted: window.__vtestBackend.posted,
            log: V.getIpcLog(),
            net: V.getNetworkLog(),
            status: V.getIpcCaptureStatus(),
            diag: V.getDiagnostics(),
            events: V.getEventStream(0).filter(function (e) {{ return e.type === 'ipc'; }}),
            netEvents: V.getEventStream(0).filter(function (e) {{ return e.type === 'network'; }}).length,
        }};"
    );
    let Some(r) = run_one("fallback", "{ failCustomProtocol: true }", &code) else {
        return;
    };
    // The app is unaffected by the observation.
    assert_eq!(r["app"]["greet"], "Hello, Ada!", "{r}");
    assert_eq!(r["app"]["fail"], "boom", "{r}");
    assert_eq!(r["app"]["add"]["sum"], 3, "{r}");
    assert_eq!(r["fetched"], 0, "{r}");
    // Every app call went over postMessage, flagged as a blocked-protocol retry. (Victauri's
    // own bridge-ready signal is usually the first call to hit the failing protocol.)
    let app_posts: Vec<&serde_json::Value> = r["posted"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| !p["cmd"].as_str().unwrap().starts_with("plugin:victauri|"))
        .collect();
    assert_eq!(app_posts.len(), 3, "{r}");
    assert!(app_posts.iter().all(|p| p["blocked"] == true), "{r}");

    // The IPC log is complete: one entry per logical call, tagged, with bodies + outcome.
    let log = r["log"].as_array().expect("log array");
    let cmds: Vec<&str> = log.iter().map(|e| e["command"].as_str().unwrap()).collect();
    assert_eq!(
        cmds,
        ["greet", "fail", "add"],
        "the failed first fetch must not appear as a separate IPC call: {r}"
    );
    for e in log {
        assert_eq!(e["transport"], "post_message", "{r}");
        assert!(e["duration_ms"].is_number(), "{r}");
    }
    assert_eq!(log[0]["status"], "ok", "{r}");
    assert_eq!(log[0]["args"]["name"], "Ada", "{r}");
    assert_eq!(log[0]["result"], "Hello, Ada!", "{r}");
    assert_eq!(log[1]["status"], "error", "{r}");
    assert_eq!(log[1]["error"], "boom", "{r}");
    assert_eq!(log[2]["result"]["sum"], 3, "{r}");
    assert_eq!(log[2]["args"]["b"], 2, "{r}");

    // The failed custom-protocol attempt is still visible as network traffic.
    let net = r["net"].as_array().unwrap();
    assert!(
        net.iter().any(|n| n["status"] == "error"
            && n["url"]
                .as_str()
                .unwrap()
                .starts_with("http://ipc.localhost/greet")),
        "{r}"
    );

    // Degradation is reported, with the capture level.
    assert_eq!(r["status"]["transport"], "post_message_fallback", "{r}");
    assert_eq!(r["status"]["capture"], "full", "{r}");
    assert_eq!(
        r["status"]["fallback_detected_via"], "console_warning",
        "{r}"
    );
    assert_eq!(r["status"]["post_message_calls"], 3, "{r}");
    assert_eq!(r["status"]["post_message_responses"], 3, "{r}");
    let warn = r["diag"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == "ipc-postmessage-fallback")
        .unwrap_or_else(|| panic!("no fallback warning: {r}"));
    assert_eq!(warn["severity"], "low", "{r}");

    // The recording stream carries the transport (so replay can skip these).
    let events = r["events"].as_array().unwrap();
    assert_eq!(events.len(), 3, "{r}");
    assert!(
        events.iter().all(|e| e["transport"] == "post_message"),
        "{r}"
    );
}

#[test]
fn invoke_key_is_never_retained() {
    let code = format!(
        "{DRIVE_THREE}
        var V = window.__VICTAURI__;
        return JSON.stringify([V.getIpcLog(), V.getNetworkLog(), V.getEventStream(0),
                               V.getIpcCaptureStatus()]).indexOf('vtest-invoke-key') === -1;"
    );
    let Some(r) = run_one("invoke key", "{ failCustomProtocol: true }", &code) else {
        return;
    };
    assert_eq!(r, true);
}

#[test]
fn android_always_posts_and_is_captured() {
    let code = format!(
        "{DRIVE_THREE}
        var V = window.__VICTAURI__;
        return {{ app: out, fetched: window.__vtestBackend.fetched.length,
                 log: V.getIpcLog(), status: V.getIpcCaptureStatus() }};"
    );
    let Some(r) = run_one("android", "{ os: 'android' }", &code) else {
        return;
    };
    assert_eq!(r["app"]["greet"], "Hello, Ada!", "{r}");
    assert_eq!(r["fetched"], 0, "{r}");
    assert_eq!(r["log"].as_array().unwrap().len(), 3, "{r}");
    assert_eq!(r["log"][0]["transport"], "post_message", "{r}");
    assert_eq!(r["status"]["transport"], "post_message", "{r}");
    assert_eq!(r["status"]["capture"], "full", "{r}");
    assert!(r["status"]["fallback_detected_via"].is_null(), "{r}");
}

#[test]
fn without_the_callback_table_requests_are_captured_and_marked() {
    let code = format!(
        "{DRIVE_THREE}
        var V = window.__VICTAURI__;
        var idle = await V.waitFor({{ condition: 'ipc_idle', timeout_ms: 500, poll_ms: 20 }});
        return {{ app: out, log: V.getIpcLog(), status: V.getIpcCaptureStatus(),
                 diag: V.getDiagnostics(), idle: idle }};"
    );
    let Some(r) = run_one(
        "requests only",
        "{ failCustomProtocol: true, hideCallbacks: true }",
        &code,
    ) else {
        return;
    };
    assert_eq!(r["app"]["greet"], "Hello, Ada!", "{r}");
    let log = r["log"].as_array().unwrap();
    assert_eq!(log.len(), 3, "{r}");
    assert!(log.iter().all(|e| e["status"] == "unknown"), "{r}");
    assert_eq!(log[0]["args"]["name"], "Ada", "{r}");
    assert_eq!(r["status"]["capture"], "requests_only", "{r}");
    assert_eq!(
        r["idle"]["ok"], true,
        "an unobservable call must not wedge ipc_idle: {r}"
    );
    let warn = r["diag"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == "ipc-postmessage-fallback")
        .unwrap_or_else(|| panic!("no fallback warning: {r}"));
    assert_eq!(warn["severity"], "high", "{r}");
}

#[test]
fn lookalike_messages_and_victauri_plumbing_are_not_logged() {
    let code = r"
        var I = window.__TAURI_INTERNALS__;
        var V = window.__VICTAURI__;
        // Force the switch, then exercise Victauri's own plumbing over postMessage.
        await I.invoke('greet', { name: 'x' });
        await I.invoke('plugin:victauri|victauri_eval_callback', { id: 'x', result: 'null' });
        // A page serializing a look-alike message (no real invoke) to plant a fake call.
        JSON.stringify({ cmd: 'quit_app', callback: 1, error: 2, payload: {},
                         __TAURI_INVOKE_KEY__: 'x' }, function (k, v) { return v; });
        await new Promise(function (r) { setTimeout(r, 20); });
        return { cmds: V.getIpcLog().map(function (e) { return e.command; }),
                 status: V.getIpcCaptureStatus() };
    ";
    let Some(r) = run_one("lookalike", "{ failCustomProtocol: true }", code) else {
        return;
    };
    assert_eq!(r["cmds"], serde_json::json!(["greet"]), "{r}");
    assert_eq!(r["status"]["post_message_calls"], 1, "{r}");
}

#[test]
fn json_stringify_stays_transparent() {
    let code = r"
        var circ = {}; circ.self = circ;
        var threw = null;
        try { JSON.stringify(circ, function (k, v) { return v; }); } catch (e) { threw = e instanceof TypeError; }
        return {
            plain: JSON.stringify({ a: 1, b: [1, 2] }),
            replacerFn: JSON.stringify({ a: 1, b: 2 }, function (k, v) { return k === 'b' ? undefined : v; }),
            replacerArr: JSON.stringify({ a: 1, b: 2 }, ['b']),
            space: JSON.stringify({ a: 1 }, null, 2),
            undef: JSON.stringify(undefined) === undefined,
            str: JSON.stringify('s'),
            length: JSON.stringify.length,
            circularThrows: threw,
        };
    ";
    let Some(r) = run_one("transparent", "{}", code) else {
        return;
    };
    assert_eq!(r["plain"], r#"{"a":1,"b":[1,2]}"#, "{r}");
    assert_eq!(r["replacerFn"], r#"{"a":1}"#, "{r}");
    assert_eq!(r["replacerArr"], r#"{"b":2}"#, "{r}");
    assert_eq!(r["space"], "{\n  \"a\": 1\n}", "{r}");
    assert_eq!(r["undef"], true, "{r}");
    assert_eq!(r["str"], "\"s\"", "{r}");
    assert_eq!(r["length"], 3, "{r}");
    assert_eq!(r["circularThrows"], true, "{r}");
}

#[test]
fn oversized_postmessage_bodies_are_bounded_and_clear_works() {
    let code = r"
        var I = window.__TAURI_INTERNALS__;
        var V = window.__VICTAURI__;
        await I.invoke('big', { blob: new Array(70001).join('y') });
        await new Promise(function (r) { setTimeout(r, 20); });
        var e = V.getIpcLog()[0];
        var before = V.getIpcLog().length;
        V._agent('__AGENT_KEY__').clearIpcLog();
        return { args: e.args, result: e.result, size: e.arg_size_bytes === undefined ? null : e.arg_size_bytes,
                 before: before, after: V.getIpcLog().length };
    ";
    let Some(r) = run_one("bounded", "{ failCustomProtocol: true }", code) else {
        return;
    };
    assert!(
        r["args"]
            .as_str()
            .is_some_and(|s| s.starts_with("[body omitted:")),
        "{r}"
    );
    assert!(
        r["result"]
            .as_str()
            .is_some_and(|s| s.starts_with("[body omitted:")),
        "{r}"
    );
    assert_eq!(r["before"], 1, "{r}");
    assert_eq!(r["after"], 0, "{r}");
}

/// A `route` rule on an IPC URL controls what the page's real `invoke` gets (fetch
/// transport): `fulfill` answers it in the page — the backend never runs — and `delay` holds
/// it back. The log must say so, classify a mocked call the way Tauri does (resolved only on
/// `Tauri-Response: ok`), and the evidence views must leave mocked calls out: otherwise a
/// mocked success would "verify" a handler that does not exist.
#[test]
fn route_answered_ipc_is_marked_and_kept_out_of_evidence() {
    let code = r"
        var I = window.__TAURI_INTERNALS__;
        var V = window.__VICTAURI__;
        V.addRoute({ pattern: 'ipc.localhost/ghost_cmd', action: 'fulfill',
                     body: JSON.stringify('forged'), headers: { 'Tauri-Response': 'ok' } });
        V.addRoute({ pattern: 'ipc.localhost/greet', action: 'fulfill', body: JSON.stringify('mock error') });
        V.addRoute({ pattern: 'ipc.localhost/add', action: 'delay', delay_ms: 30 });
        var out = {};
        out.ghost = await I.invoke('ghost_cmd', {});
        try { await I.invoke('greet', { name: 'Ada' }); out.greet = 'resolved?'; } catch (e) { out.greet = e; }
        out.add = await I.invoke('add', { a: 1, b: 2 });
        await new Promise(function (r) { setTimeout(r, 20); });
        function view(opts) { return V.getIpcLog(0, opts).map(function (e) { return e.command; }); }
        return {
            app: out,
            backendSaw: window.__vtestBackend.fetched,
            log: V.getIpcLog(),
            backendOnly: view({ bodies: false, backend_only: true }),
            unrouted: view({ bodies: false, unrouted: true }),
            status: V.getIpcCaptureStatus(),
        };
    ";
    let Some(r) = run_one("route on ipc", "{}", code) else {
        return;
    };
    // The page got exactly what the rules said, and the backend saw only the delayed call.
    assert_eq!(r["app"]["ghost"], "forged", "{r}");
    assert_eq!(r["app"]["greet"], "mock error", "{r}");
    assert_eq!(r["app"]["add"]["sum"], 3, "{r}");
    let fetched = r["backendSaw"].to_string();
    assert!(
        !fetched.contains("ghost_cmd") && !fetched.contains("greet"),
        "{r}"
    );
    assert!(fetched.contains("add"), "{r}");

    let log = r["log"].as_array().expect("log array");
    assert_eq!(log.len(), 3, "{r}");
    let (ghost, greet, add) = (&log[0], &log[1], &log[2]);
    assert_eq!(ghost["mocked"], true, "{r}");
    assert_eq!(ghost["status"], "ok", "Tauri-Response: ok resolves: {r}");
    assert!(ghost["route_rule"].is_number(), "{r}");
    assert_eq!(greet["mocked"], true, "{r}");
    assert_eq!(
        greet["status"], "error",
        "no Tauri-Response: ok means the page's invoke rejected: {r}"
    );
    assert!(add.get("mocked").is_none(), "{r}");
    assert_eq!(add["route_delay_ms"], 30, "{r}");
    assert_eq!(add["status"], "ok", "{r}");

    // Evidence views: mocked calls never count; timings also drop the delayed call.
    assert_eq!(r["backendOnly"], serde_json::json!(["add"]), "{r}");
    assert_eq!(r["unrouted"], serde_json::json!([]), "{r}");
    // A mocked response does not switch Tauri's transport.
    assert_eq!(r["status"]["transport"], "fetch", "{r}");
}

#[test]
fn a_forged_fallback_warning_reports_degraded_never_healthy() {
    let code = r"
        console.warn('IPC custom protocol failed, Tauri will now use the postMessage interface instead');
        return window.__VICTAURI__.getIpcCaptureStatus();
    ";
    let Some(r) = run_one("forged warning", "{}", code) else {
        return;
    };
    assert_eq!(r["transport"], "post_message_fallback", "{r}");
    assert_eq!(r["capture"], "none", "{r}");
}

/// An app with `useHttpsScheme` (Windows, Android) sends IPC to `https://ipc.localhost/`. The
/// bridge used to recognise only http:// and ipc://, so such an app's IPC log was empty and
/// `ipc_capture` said "no IPC observed yet" while it made calls (red team H1).
#[test]
fn https_scheme_ipc_is_captured_and_route_aware() {
    let code = format!(
        "{DRIVE_THREE}
        var V = window.__VICTAURI__;
        var rule = V.addRoute({{ pattern: 'https://ipc.localhost/never_called', match_type: 'exact',
                                 action: 'delay', delay_ms: 1 }});
        return {{ app: out, fetched: window.__vtestBackend.fetched, log: V.getIpcLog(),
                 status: V.getIpcCaptureStatus(), note: rule.ipc_note || null }};"
    );
    let Some(r) = run_one("https scheme", "{ httpsScheme: true }", &code) else {
        return;
    };
    assert_eq!(r["app"]["greet"], "Hello, Ada!", "{r}");
    assert!(r["fetched"].to_string().contains("greet"), "{r}");
    let cmds: Vec<&str> = r["log"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["command"].as_str().unwrap())
        .collect();
    assert_eq!(cmds, ["greet", "fail", "add"], "{r}");
    assert_eq!(r["status"]["transport"], "fetch", "{r}");
    assert!(
        r["note"].is_string(),
        "a rule on an https IPC URL is recognised as IPC: {r}"
    );
}

/// Page script can register a callback pair itself (`transformCallback` is public), so a
/// postMessage-shaped message proves nothing. It must not hide or rename a real fetch call
/// that is still in flight (red team F1, T2), and Tauri's callback ids stay out of
/// `getNetworkLog` (they let a page re-plant a completed call's ids — T3).
#[test]
fn staged_postmessage_lookalikes_cannot_hide_or_rename_real_calls() {
    let code = r"
        var I = window.__TAURI_INTERNALS__;
        var V = window.__VICTAURI__;
        var p = I.invoke('fail', {});
        var keys = Array.from(I.callbacks.keys());
        JSON.stringify({ cmd: 'harmless_cmd', callback: keys[keys.length - 2], error: keys[keys.length - 1],
                         payload: {}, options: { customProtocolIpcBlocked: true }, __TAURI_INVOKE_KEY__: 'x' },
                       function (k, v) { return v; });
        try { await p; } catch (e) {}
        await new Promise(function (r) { setTimeout(r, 20); });
        var net = V.getNetworkLog(null, 0, {});
        return {
            log: V.getIpcLog().map(function (e) { return [e.command, e.status, e.transport || 'fetch']; }),
            status: V.getIpcCaptureStatus(),
            netHasCallbackIds: net.some(function (n) { return 'ipc_callback' in n; }),
        };
    ";
    let Some(r) = run_one("lookalikes", "{}", code) else {
        return;
    };
    assert_eq!(
        r["log"],
        serde_json::json!([["fail", "error", "fetch"]]),
        "the real call must stay, unrenamed, and the look-alike must not be logged: {r}"
    );
    assert_eq!(
        r["status"]["transport"], "fetch",
        "a staged blocked flag must not fake a fallback: {r}"
    );
    assert_eq!(r["netHasCallbackIds"], false, "{r}");
}

/// A call page script stages from scratch is logged (on Android it could be real) but only as
/// postMessage evidence, which ghost detection never counts as proof (red team F1, T1).
#[test]
fn a_page_staged_postmessage_call_is_tagged_unproven() {
    let code = r"
        var I = window.__TAURI_INTERNALS__;
        var V = window.__VICTAURI__;
        var cb = I.transformCallback(function () {}, true), er = I.transformCallback(function () {}, true);
        JSON.stringify({ cmd: 'definitely_not_a_command', callback: cb, error: er, payload: {},
                         options: {}, __TAURI_INVOKE_KEY__: 'bogus' }, function (k, v) { return v; });
        I.runCallback(cb, { forged: true });
        await new Promise(function (r) { setTimeout(r, 20); });
        return V.getIpcLog(0, { bodies: false, backend_only: true });
    ";
    let Some(r) = run_one("staged", "{}", code) else {
        return;
    };
    let e = &r[0];
    assert_eq!(e["command"], "definitely_not_a_command", "{r}");
    assert_eq!(
        e["transport"], "post_message",
        "must carry the unproven tag ghost detection keys on: {r}"
    );
}

/// End to end through the real analysis projections: a staged postMessage "success" lands in
/// the ghost outcomes only as `pm_ok` (never `ok`), and `command_catalog` flags shapes it could
/// learn only from postMessage — while a real fetch call stays proven evidence.
#[test]
fn analyses_keep_page_stageable_postmessage_evidence_apart() {
    use victauri_plugin::mcp::ipc_log_js::{ghost_ipc_outcomes_js, ipc_catalog_projection_js};
    let ghost = ghost_ipc_outcomes_js(None);
    let catalog = ipc_catalog_projection_js();
    let code = format!(
        "var I = window.__TAURI_INTERNALS__;
        await I.invoke('greet', {{ name: 'Ada' }});
        var cb = I.transformCallback(function () {{}}, true), er = I.transformCallback(function () {{}}, true);
        JSON.stringify({{ cmd: 'staged_cmd', callback: cb, error: er, payload: {{ x: 1 }}, options: {{}},
                         __TAURI_INVOKE_KEY__: 'bogus' }}, function (k, v) {{ return v; }});
        I.runCallback(cb, {{ forged: true }});
        await new Promise(function (r) {{ setTimeout(r, 20); }});
        var ghost = (function () {{ {ghost} }})();
        var catalog = (function () {{ {catalog} }})();
        return {{ ghost: ghost, catalog: catalog }};"
    );
    let Some(r) = run_one("analyses", "{}", &code) else {
        return;
    };
    let pick = |list: &serde_json::Value, cmd: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|e| e["command"] == cmd)
            .cloned()
            .unwrap_or_else(|| panic!("{cmd} missing: {r}"))
    };
    let staged = pick(&r["ghost"], "staged_cmd");
    assert_eq!(staged["ok"], false, "{r}");
    assert_eq!(staged["pm_ok"], true, "{r}");
    assert_eq!(pick(&r["ghost"], "greet")["ok"], true, "{r}");
    assert_eq!(
        pick(&r["catalog"], "staged_cmd")["shapes_from_post_message"],
        true,
        "{r}"
    );
    assert!(
        pick(&r["catalog"], "greet")
            .get("shapes_from_post_message")
            .is_none(),
        "{r}"
    );
}

/// The stringify observer is as unconstructible as the native function (red team F5), and a
/// page that replaces it is reported instead of "full" capture (red team F2).
#[test]
fn stringify_observer_is_native_shaped_and_its_removal_is_reported() {
    let code = r"
        var V = window.__VICTAURI__;
        var threw = false;
        try { new JSON.stringify({}); } catch (e) { threw = e instanceof TypeError; }
        var proto = Object.prototype.hasOwnProperty.call(JSON.stringify, 'prototype');
        var before = V.getIpcCaptureStatus().post_message_hook;
        JSON.stringify = function () { return 'null'; };
        var after = V.getIpcCaptureStatus();
        return { newThrows: threw, hasPrototype: proto, before: before, after: after };
    ";
    let Some(r) = run_one("observer shape", "{}", code) else {
        return;
    };
    assert_eq!(r["newThrows"], true, "{r}");
    assert_eq!(r["hasPrototype"], false, "{r}");
    assert_eq!(r["before"], "installed", "{r}");
    assert_eq!(r["after"]["post_message_hook"], "replaced", "{r}");
    assert!(
        r["after"]["note"]
            .as_str()
            .unwrap()
            .contains("replaced JSON.stringify"),
        "{r}"
    );
}

/// IPC mocks Tauri could not parse are refused — the REAL command would run over postMessage
/// (red team F3); the logged outcome follows Tauri's own reading of the response (F4); and the
/// returned rule is a copy (F6).
#[test]
fn ipc_mocks_tauri_cannot_read_are_refused_and_outcomes_match_tauri() {
    let code = r"
        var I = window.__TAURI_INTERNALS__;
        var V = window.__VICTAURI__;
        var refused = [
            V.addRoute({ pattern: 'ipc.localhost/greet', action: 'fulfill', body: 'not json' }),
            V.addRoute({ pattern: 'ipc.localhost/greet', action: 'fulfill', status: 204 }),
            V.addRoute({ pattern: 'ipc.localhost/greet', action: 'fulfill' }),
        ].map(function (r) { return r.ok; });
        var notes = [
            V.addRoute({ pattern: 'ipc.localhost/zzz', action: 'delay', delay_ms: 1 }).ipc_note,
            V.addRoute({ pattern: '*', match_type: 'glob', action: 'delay', delay_ms: 1, method: 'GET' }).ipc_note,
            V.addRoute({ pattern: '/api/users', action: 'delay', delay_ms: 1 }).ipc_note,
        ].map(function (n) { return typeof n === 'string'; });
        V._agent('__AGENT_KEY__').clearRoutes();
        var textOk = V.addRoute({ pattern: 'ipc.localhost/add', action: 'fulfill', body: 'plain',
                                  content_type: 'text/plain', headers: { 'Tauri-Response': 'ok' } });
        textOk.rule.delay_ms = 999999;
        var stored = V.getRouteRules().filter(function (r) { return r.id === textOk.id; })[0].delay_ms;
        var nbsp = V.addRoute({ pattern: 'ipc.localhost/greet', action: 'fulfill', body: JSON.stringify('x'),
                                headers: { 'Tauri-Response': 'ok ' } });
        var out = {};
        out.add = await I.invoke('add', { a: 1, b: 2 });
        try { await I.invoke('greet', { name: 'x' }); out.greet = 'resolved?'; } catch (e) { out.greet = 'rejected'; }
        await new Promise(function (r) { setTimeout(r, 20); });
        return { refused: refused, notes: notes, textOk: textOk.ok, stored: stored, nbspOk: nbsp.ok, app: out,
                 log: V.getIpcLog().map(function (e) { return [e.command, e.status, e.result]; }) };
    ";
    let Some(r) = run_one("mock validity", "{}", code) else {
        return;
    };
    assert_eq!(
        r["refused"],
        serde_json::json!([false, false, false]),
        "{r}"
    );
    assert_eq!(
        r["notes"],
        serde_json::json!([true, false, false]),
        "a command-specific IPC rule gets the note; a GET-only or non-IPC rule does not: {r}"
    );
    assert_eq!(r["textOk"], true, "{r}");
    assert_eq!(
        r["stored"], 0,
        "editing the returned rule must not change the stored one: {r}"
    );
    assert_eq!(r["nbspOk"], true, "{r}");
    assert_eq!(r["app"]["add"], "plain", "{r}");
    assert_eq!(
        r["app"]["greet"], "rejected",
        "Tauri compares the header exactly: {r}"
    );
    assert_eq!(
        r["log"],
        serde_json::json!([["add", "ok", "plain"], ["greet", "error", "x"]]),
        "{r}"
    );
}
