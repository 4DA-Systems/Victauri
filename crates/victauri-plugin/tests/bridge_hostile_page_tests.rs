//! The IPC evidence the bridge records vs a hostile page (jsdom via Node.js).
//!
//! Page script shares the bridge's realm. These pin what it can NOT do to the IPC log, the
//! route rules and the analyses built on them (ghost detection, `command_catalog`, coverage,
//! timings) — found by the 0.10.0 adversarial audit, each reproduced before its fix:
//! an IPC mock Tauri cannot read must never run the real command; a mocked call's ids cannot
//! be reused to erase it; a 200 without `Tauri-Response` (a preflight) proves no handler;
//! `Array.prototype` / `Headers.prototype` hooks get no live log entry and forge no outcome.
//!
//! Same harness as `bridge_ipc_transport_tests.rs` (Tauri's real `core.js` + `ipc-protocol.js`
//! with an emulated backend, loaded before the bridge); `VICTAURI_REQUIRE_JSDOM=1` (CI sets
//! `CI`) makes a missing jsdom fail instead of skipping.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use victauri_plugin::js_bridge::{BridgeCapacities, agent_key, init_script};
use victauri_plugin::mcp::ipc_log_js::ghost_ipc_outcomes_js;

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

/// Run one case (after Tauri's fixture and `extra_pre`) and return its result value, or `None`
/// when jsdom is not installed outside CI.
fn run_one(name: &str, config: &str, extra_pre: &str, code: &str) -> Option<serde_json::Value> {
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
        tests: vec![TestCase {
            name: name.to_string(),
            code: code.to_string(),
            pre_bridge_js: format!("window.__vtestTauriConfig = {config};\n{fixture}\n{extra_pre}"),
            node_web_api: true,
        }],
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
    let line = stdout
        .lines()
        .find(|l| l.starts_with("VICTAURI_RESULTS:"))
        .unwrap_or_else(|| {
            panic!(
                "No VICTAURI_RESULTS line.\nstdout: {stdout}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
    let results: Vec<TestResult> = serde_json::from_str(&line["VICTAURI_RESULTS:".len()..])
        .unwrap_or_else(|e| panic!("bad results JSON: {e}\nraw: {line}"));
    let r = results.into_iter().next().expect("one result");
    assert!(r.passed, "{}: {:?}", r.name, r.error);
    Some(r.result.unwrap_or(serde_json::Value::Null))
}

/// The ghost-detection outcome (`ok` flag) for `command`, from a result's `outcomes`.
fn outcome_ok(r: &serde_json::Value, command: &str) -> Option<serde_json::Value> {
    r["outcomes"]
        .as_array()
        .expect("outcomes")
        .iter()
        .find(|o| o["command"] == command)
        .map(|o| o["ok"].clone())
}

/// Emulates Tauri's protocol handler for non-POST methods (tauri `ipc/protocol.rs`: OPTIONS →
/// 200, empty body, NO `Tauri-Response`; anything else → 405). The fixture answers every
/// method like POST, so this wraps it.
const NON_POST_PROTOCOL: &str = r"
(function () {
  var f = window.fetch;
  window.fetch = function (input, init) {
    var url = typeof input === 'string' ? input : String(input);
    var m = (init && init.method) ? String(init.method).toUpperCase() : 'GET';
    if (/^(?:https?:\/\/ipc\.localhost|ipc:\/\/localhost)\//.test(url) && m !== 'POST') {
      if (m === 'OPTIONS') return Promise.resolve(new Response(null, { status: 200,
        headers: { 'Access-Control-Allow-Headers': '*' } }));
      return Promise.resolve(new Response('only POST and OPTIONS are allowed', { status: 405,
        headers: { 'Content-Type': 'text/plain' } }));
    }
    return f.apply(this, arguments);
  };
})();
";

/// Every Tauri POST answer that reached a handler carries `Tauri-Response`; a 200 without it
/// (an OPTIONS preflight a page can send to any IPC URL) ran no command, so it must not
/// "verify" a command that has no handler.
#[test]
fn a_response_without_tauri_response_proves_no_handler() {
    let ghost = ghost_ipc_outcomes_js(None);
    let code = format!(
        "var I = window.__TAURI_INTERNALS__;
        try {{ await I.invoke('ghost_cmd', {{}}); }} catch (e) {{}}
        await fetch('http://ipc.localhost/ghost_cmd', {{ method: 'OPTIONS' }});
        await new Promise(function (r) {{ setTimeout(r, 20); }});
        return {{ outcomes: (function () {{ {ghost} }})(),
                 log: window.__VICTAURI__.getIpcLog().map(function (e) {{ return e.status; }}) }};"
    );
    let Some(r) = run_one("no_tauri_response", "{}", NON_POST_PROTOCOL, &code) else {
        return;
    };
    assert_eq!(
        outcome_ok(&r, "ghost_cmd"),
        Some(serde_json::json!(false)),
        "{r}"
    );
    assert_eq!(r["log"], serde_json::json!(["error", "unknown"]), "{r}");
}

/// An IPC mock Tauri cannot read makes Tauri run the REAL command over postMessage (and stay
/// there). `add` refuses one that names the IPC host however its content type is spelled
/// (`Headers` trims it) and any mock the Response constructor rejects; a broader pattern
/// (`greet`) is accepted with a warning, and a matching IPC call gets a Tauri error from
/// Victauri — the real command never runs and the page stays on fetch.
#[test]
fn an_unreadable_ipc_mock_never_runs_the_real_command() {
    let code = r"
        var I = window.__TAURI_INTERNALS__, V = window.__VICTAURI__, out = {};
        out.spaced = V.addRoute({ pattern: 'ipc.localhost/greet', action: 'fulfill', body: 'not json',
                                  content_type: ' application/json' });
        out.bad_header = V.addRoute({ pattern: 'ipc.localhost/greet', action: 'fulfill', body: '\x22x\x22',
                                      headers: { 'Tauri-Response': 'ok', 'Bad Header': '1' } });
        out.bad_text = V.addRoute({ pattern: '/api/x', action: 'fulfill', body: '{}', status_text: 'café☃' });
        var broad = V.addRoute({ pattern: 'greet', action: 'fulfill', body: 'hello' });
        out.broad_ok = broad.ok; out.broad_warning = typeof broad.ipc_warning;
        try { out.greet = await I.invoke('greet', { name: 'Ada' }); } catch (e) { out.greet = 'rejected: ' + e; }
        await new Promise(function (r) { setTimeout(r, 30); });
        var log = V.getIpcLog();
        return { out: out,
                 posted: window.__vtestBackend.posted.map(function (p) { return p.cmd; }),
                 transport: V.getIpcCaptureStatus().transport,
                 entry: log[log.length - 1] };
    ";
    let Some(r) = run_one("unreadable_mock", "{}", "", code) else {
        return;
    };
    let out = &r["out"];
    assert_eq!(out["spaced"]["ok"], false, "{r}");
    assert_eq!(out["bad_header"]["ok"], false, "{r}");
    assert!(
        out["bad_header"]["error"]
            .as_str()
            .unwrap()
            .contains("cannot be built"),
        "{r}"
    );
    assert_eq!(out["bad_text"]["ok"], false, "{r}");
    assert_eq!(out["broad_ok"], true, "{r}");
    assert_eq!(out["broad_warning"], "string", "{r}");
    assert!(
        out["greet"]
            .as_str()
            .unwrap()
            .starts_with("rejected: victauri: route #"),
        "{r}"
    );
    assert!(
        !r["posted"].to_string().contains("greet"),
        "the real command ran over postMessage: {r}"
    );
    assert_eq!(r["transport"], "fetch", "{r}");
    assert_eq!(r["entry"]["mocked"], true, "{r}");
    assert_eq!(r["entry"]["status"], "error", "{r}");
    assert!(r["entry"]["mock_unreadable"].is_string(), "{r}");
}

/// A mocked call is answered and never retried, so page script that re-plants its callback ids
/// (Tauri's callbacks Map is page-writable) and stages a look-alike — even one claiming the
/// custom protocol was blocked — can neither erase it, nor rename it, nor fake a fallback.
#[test]
fn a_mocked_call_cannot_be_erased_by_reusing_its_ids() {
    let code = r"
        var I = window.__TAURI_INTERNALS__, V = window.__VICTAURI__;
        V.addRoute({ pattern: 'ipc.localhost/greet', action: 'fulfill', delay_ms: 10,
                     body: JSON.stringify('mocked'), headers: { 'Tauri-Response': 'ok' } });
        var p = I.invoke('greet', { name: 'Ada' });
        var keys = Array.from(I.callbacks.keys());
        var cb = keys[keys.length - 2], er = keys[keys.length - 1];
        var got = await p;
        await new Promise(function (r) { setTimeout(r, 20); });
        I.callbacks.set(cb, function () {}); I.callbacks.set(er, function () {});
        JSON.stringify({ cmd: 'harmless_cmd', callback: cb, error: er, payload: {},
                         options: { customProtocolIpcBlocked: true }, __TAURI_INVOKE_KEY__: 'x' },
                       function (k, v) { return v; });
        return { got: got,
                 after: V.getIpcLog().map(function (e) { return [e.command, e.transport || 'fetch', !!e.mocked]; }),
                 status: V.getIpcCaptureStatus().transport };
    ";
    let Some(r) = run_one("mocked_ids", "{}", "", code) else {
        return;
    };
    assert_eq!(r["got"], "mocked", "{r}");
    assert_eq!(
        r["after"],
        serde_json::json!([["greet", "fetch", true]]),
        "{r}"
    );
    assert_eq!(
        r["status"], "fetch",
        "a staged blocked flag faked a fallback: {r}"
    );
}

/// During Tauri's genuine fetch → postMessage retry, page code that runs in between (Tauri
/// calls `console.warn` first) can serialize a look-alike under the REAL call's ids. Which of
/// the two messages is genuine is unknowable from inside the page, so the entry is flagged
/// `ids_contested` with both names instead of silently carrying the page's choice.
#[test]
fn ids_claimed_by_two_commands_are_flagged_contested() {
    let code = r"
        var I = window.__TAURI_INTERNALS__, V = window.__VICTAURI__;
        console.warn = function () {
            var keys = Array.from(I.callbacks.keys());
            JSON.stringify({ cmd: 'harmless_cmd', callback: keys[keys.length - 2], error: keys[keys.length - 1],
                             payload: {}, options: {}, __TAURI_INVOKE_KEY__: 'x' }, function (k, v) { return v; });
        };
        var got = await I.invoke('greet', { name: 'Ada' });
        await new Promise(function (r) { setTimeout(r, 30); });
        return { got: got, log: V.getIpcLog() };
    ";
    let Some(r) = run_one("contested", "{ failCustomProtocol: true }", "", code) else {
        return;
    };
    let pm: Vec<&serde_json::Value> = r["log"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["transport"] == "post_message")
        .collect();
    assert_eq!(pm.len(), 1, "{r}");
    assert_eq!(pm[0]["ids_contested"], true, "{r}");
    assert_eq!(
        pm[0]["contested_commands"],
        serde_json::json!(["harmless_cmd", "greet"]),
        "{r}"
    );
}

/// The bridge's logs are appended without `Array.prototype.push`: a page hook there must not
/// receive the live entries / rules (it could strip `mocked`, flip an outcome, or rewrite the
/// agent's stored rule behind the copy-on-read hardening).
#[test]
fn array_prototype_hooks_get_no_live_log_entry() {
    let ghost = ghost_ipc_outcomes_js(None);
    let code = format!(
        "var I = window.__TAURI_INTERNALS__, V = window.__VICTAURI__;
        var live = [];
        var push = Array.prototype.push, shift = Array.prototype.shift, forEach = Array.prototype.forEach;
        Array.prototype.push = function (x) {{
            if (x && typeof x === 'object' && ('timestamp' in x || 'pattern' in x)) live[live.length] = x;
            return push.apply(this, arguments);
        }};
        Array.prototype.forEach = function () {{ live[live.length] = this; return forEach.apply(this, arguments); }};
        V.addRoute({{ pattern: 'ipc.localhost/greet', action: 'fulfill', body: JSON.stringify('m'),
                      headers: {{ 'Tauri-Response': 'ok' }} }});
        await I.invoke('greet', {{ name: 'x' }});
        try {{ await I.invoke('ghost_cmd', {{}}); }} catch (e) {{}}
        await new Promise(function (r) {{ setTimeout(r, 20); }});
        V.getIpcLog(); V.getNetworkLog(); V.getRouteMatches(5);
        Array.prototype.push = push; Array.prototype.forEach = forEach;
        live.forEach(function (e) {{
            if (e.mocked) delete e.mocked;
            if (e.url && e.url.indexOf('ghost_cmd') !== -1) e.ipc_response = 'ok';
            if (e.pattern) e.action = 'delay';
        }});
        return {{ outcomes: (function () {{ {ghost} }})(), rules: V.getRouteRules(),
                 captured: live.length }};"
    );
    let Some(r) = run_one("array_hooks", "{}", "", &code) else {
        return;
    };
    assert_eq!(outcome_ok(&r, "greet"), None, "{r}");
    assert_eq!(
        outcome_ok(&r, "ghost_cmd"),
        Some(serde_json::json!(false)),
        "{r}"
    );
    assert_eq!(r["rules"][0]["action"], "fulfill", "{r}");
}

/// A real response's `Tauri-Response` is read through the native `Headers.get`: a page patch
/// of `Headers.prototype.get` cannot turn "command not found" into a verified handler.
#[test]
fn a_patched_headers_get_forges_no_outcome() {
    let ghost = ghost_ipc_outcomes_js(None);
    let code = format!(
        "var I = window.__TAURI_INTERNALS__;
        var g = Headers.prototype.get;
        Headers.prototype.get = function (n) {{
            var v = g.call(this, n);
            return (String(n).toLowerCase() === 'tauri-response' && v === 'error') ? 'ok' : v;
        }};
        try {{ await I.invoke('ghost_cmd', {{}}); }} catch (e) {{}}
        Headers.prototype.get = g;
        await new Promise(function (r) {{ setTimeout(r, 20); }});
        return {{ outcomes: (function () {{ {ghost} }})() }};"
    );
    let Some(r) = run_one("headers_get", "{}", "", &code) else {
        return;
    };
    assert_eq!(
        outcome_ok(&r, "ghost_cmd"),
        Some(serde_json::json!(false)),
        "{r}"
    );
}

/// A mocked call's logged outcome is the one the app got: Tauri ignores the HTTP status and
/// resolves on `Tauri-Response: ok` alone.
#[test]
fn a_mocks_logged_outcome_follows_tauri_response_not_status() {
    let code = r"
        var I = window.__TAURI_INTERNALS__, V = window.__VICTAURI__, out = {};
        V.addRoute({ pattern: 'ipc.localhost/greet', action: 'fulfill', status: 500,
                     body: JSON.stringify('fine'), headers: { 'Tauri-Response': 'ok' } });
        try { out.app = await I.invoke('greet', { name: 'x' }); } catch (e) { out.app = 'rejected'; }
        await new Promise(function (r) { setTimeout(r, 20); });
        out.log = V.getIpcLog().map(function (e) { return [e.command, e.status]; });
        return out;
    ";
    let Some(r) = run_one("mock_outcome", "{}", "", code) else {
        return;
    };
    assert_eq!(r["app"], "fine", "{r}");
    assert_eq!(r["log"], serde_json::json!([["greet", "ok"]]), "{r}");
}

/// A spelling of an IPC URL that fetch accepts but a raw prefix check misses is still logged
/// as IPC (it reaches Tauri the same way).
#[test]
fn unusual_ipc_url_spellings_are_still_ipc() {
    let code = r"
        var V = window.__VICTAURI__;
        var urls = ['HTTP://IPC.LOCALHOST/greet', 'http://ipc.localhost:80/greet'];
        for (var i = 0; i < urls.length; i++) {
            try { await fetch(urls[i], { method: 'POST', body: '{}' }); } catch (e) {}
        }
        await new Promise(function (r) { setTimeout(r, 20); });
        return V.getIpcLog().map(function (e) { return e.command; });
    ";
    let Some(r) = run_one("url_spellings", "{}", "", code) else {
        return;
    };
    assert_eq!(r, serde_json::json!(["greet", "greet"]), "{r}");
}
