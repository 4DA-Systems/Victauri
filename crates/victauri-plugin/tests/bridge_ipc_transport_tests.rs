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
