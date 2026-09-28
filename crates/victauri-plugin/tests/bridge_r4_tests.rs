//! Round-4 audit regressions for the injected JS bridge (jsdom via Node.js).
//!
//! Same harness as `bridge_tests.rs` (`tests/bridge_tests/run_tests.js`): each test serializes
//! a batch of JS cases, runs them in a fresh jsdom with the real bridge injected, and asserts
//! on the results. Set `VICTAURI_REQUIRE_JSDOM=1` (CI sets `CI`) so a missing jsdom fails
//! instead of skipping.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use victauri_plugin::js_bridge::{
    BridgeCapacities, agent_key, agent_ops_js, eval_wrapper_script, init_script,
};

#[derive(Serialize)]
struct TestDef {
    bridge_script: String,
    setup_html: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    setup_js: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    node_web_api: bool,
    tests: Vec<TestCase>,
}

#[derive(Serialize)]
struct TestCase {
    name: String,
    code: String,
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

fn run_tests(def: &TestDef) -> Option<Vec<TestResult>> {
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
    let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
    let json = serde_json::to_string(def).expect("serialize test def");
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

fn assert_all_pass(results: &[TestResult]) {
    let failures: Vec<String> = results
        .iter()
        .filter(|r| !r.passed)
        .map(|r| format!("  FAIL: {} => {:?}", r.name, r.error))
        .collect();
    assert!(
        failures.is_empty(),
        "Test failures:\n{}",
        failures.join("\n")
    );
}

fn case(name: &str, code: &str) -> TestCase {
    TestCase {
        name: name.into(),
        code: code.into(),
    }
}

fn def(setup_js: Option<String>, tests: Vec<TestCase>) -> TestDef {
    TestDef {
        bridge_script: init_script(&BridgeCapacities::default()),
        setup_html: r#"<html lang="en"><head><title>R4</title></head><body><div id="app"></div></body></html>"#
            .to_string(),
        setup_js,
        node_web_api: false,
        tests,
    }
}

fn result(results: &[TestResult], i: usize) -> serde_json::Value {
    results[i].result.clone().unwrap_or_default()
}

// ── R4-JS1: page script cannot forge a pagehide / pageshow ───────────────────

/// A synthetic (`isTrusted === false`) `pagehide` used to wipe every captured log and
/// permanently unhook console + DOM-mutation capture. The bridge now reacts only to the
/// browser's own (trusted) page-transition events.
#[test]
fn r4_js1_synthetic_pagehide_cannot_wipe_logs_or_unhook_capture() {
    let def = def(
        None,
        vec![
            case(
                "untrusted pagehide / pageshow are ignored",
                r"
                var V = window.__VICTAURI__;
                console.log('before');
                await fetch('http://ipc.localhost/some_cmd', { method: 'POST', body: '{}' });
                var hooked = console.log;
                window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: false }));
                window.dispatchEvent(new Event('pagehide'));
                window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
                console.log('after');
                document.getElementById('app').appendChild(document.createElement('p'));
                await new Promise(function(r) { setTimeout(r, 250); });
                return {
                    console: V.getConsoleLogs().map(function(l) { return l.message; }),
                    network: V.getNetworkLog().length,
                    ipc: V.getIpcLog().length,
                    still_hooked: console.log === hooked,
                    mutations: V.getMutationLog().length,
                };
                ",
            ),
            // Control: the browser's real (trusted) events keep their documented behaviour.
            case(
                "trusted pagehide still tears down, trusted persisted pageshow restores",
                r"
                var V = window.__VICTAURI__;
                console.log('before');
                window.__vtestTrustedPageTransition('pagehide', false);
                var cleared = V.getConsoleLogs().length;
                window.__vtestTrustedPageTransition('pageshow', true);
                console.log('restored');
                return { cleared: cleared, after: V.getConsoleLogs().map(function(l) { return l.message; }) };
                ",
            ),
        ],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = result(&results, 0);
    assert_eq!(r["console"], serde_json::json!(["before", "after"]), "{r}");
    assert_eq!(r["network"], 1, "{r}");
    assert_eq!(r["ipc"], 1, "{r}");
    assert_eq!(r["still_hooked"], true, "{r}");
    assert!(r["mutations"].as_u64().unwrap_or(0) >= 1, "{r}");
    let c = result(&results, 1);
    assert_eq!(c["cleared"], 0, "{c}");
    assert_eq!(c["after"], serde_json::json!(["restored"]), "{c}");
}

// ── R4-JS2: the agent key cannot be read through `.caller` / stack frames ────

/// Sloppy page hooks that walk everything reachable from a hooked built-in: the `.caller`
/// chain (and each frame's `.arguments`), and — V8 — `Error.prepareStackTrace` call sites'
/// `getFunction()`. Every reachable function's source and argument list is recorded.
const CALLER_PROBE_SETUP: &str = r"
    window.__reach = [];
    window.__hits = 0;
    function __record(start) {
        window.__hits++;
        var f = start, hops = 0;
        while (hops++ < 16) {
            var next;
            try { next = f.caller; } catch (e) { break; }
            if (!next) break;
            try { window.__reach.push('caller: ' + Function.prototype.toString.call(next)); } catch (e) {}
            try {
                var a = next.arguments;
                if (a) for (var i = 0; i < a.length; i++) window.__reach.push('arg: ' + String(a[i]));
            } catch (e) {}
            f = next;
        }
        var prev = Error.prepareStackTrace;
        try {
            Error.prepareStackTrace = function(err, sites) {
                return sites.map(function(s) {
                    var fn = s.getFunction();
                    return fn ? Function.prototype.toString.call(fn) : null;
                });
            };
            var frames = new Error().stack;
            if (Array.isArray(frames)) frames.forEach(function(src) { if (src) window.__reach.push('frame: ' + src); });
        } catch (e) {} finally { Error.prepareStackTrace = prev; }
    }
    window.__restore = [];
    function __hookMethod(proto, name) {
        var orig = proto[name];
        proto[name] = function hooked() { __record(hooked); return orig.apply(this, arguments); };
        window.__restore.push(function() { proto[name] = orig; });
    }
    function __hookGetter(proto, name, value) {
        var d = Object.getOwnPropertyDescriptor(proto, name);
        Object.defineProperty(proto, name, { configurable: true, get: function g() { __record(g); return value; } });
        window.__restore.push(function() { if (d) Object.defineProperty(proto, name, d); else delete proto[name]; });
    }
    function __run(s) { (0, eval)(s); }
    window.__calls = [];
";

#[test]
#[allow(clippy::too_many_lines)]
fn r4_js2_agent_key_unreachable_from_page_hooks() {
    let ops = agent_ops_js();
    // The exact snippet shapes Victauri's route / logs / navigate tools send through the eval
    // wrapper (mcp/mod.rs), delivered as a top-level script like webview.eval does.
    let scripts = [
        eval_wrapper_script("r4-1", &format!("return {ops}?.clearRoute(1)")),
        eval_wrapper_script("r4-2", &format!("return {ops}?.clearRoutes()")),
        eval_wrapper_script(
            "r4-3",
            &format!(
                "return (function(){{ var b = {ops}; if (!b) return {{ ok:false, error:'bridge unavailable' }}; b.clearIpcLog(); b.clearNetworkLog(); return {{ ok:true, cleared:['ipc','network'] }}; }})()"
            ),
        ),
        eval_wrapper_script(
            "r4-4",
            &format!("return {ops}?.setDialogAutoResponse(\"confirm\", \"accept\", undefined)"),
        ),
    ];
    let mut setup = String::from(CALLER_PROBE_SETUP);
    // The scripts (which carry the key) are defined at top level here, never inside a
    // function the hooks could reach — only Victauri's own injected code carries the key.
    setup.push_str(&format!(
        "window.__S = {};\n",
        serde_json::to_string(&scripts).unwrap()
    ));
    setup.push_str(
        r"
        window.__TAURI_INTERNALS__ = { invoke: function tauriInvoke(cmd, a) {
            __record(tauriInvoke); window.__calls.push(a); return Promise.resolve(null);
        } };
        ",
    );
    let def = def(
        Some(setup),
        vec![case(
            "hooked built-ins never reach the agent key or an agent-only op",
            r"
            var V = window.__VICTAURI__;
            V.addRoute({ pattern: 'never-matches-1', action: 'block' });
            V.addRoute({ pattern: 'never-matches-2', action: 'block' });
            await fetch('http://ipc.localhost/some_cmd', { method: 'POST', body: '{}' });
            __hookMethod(Array.prototype, 'filter');
            __hookMethod(Array.prototype, 'splice');
            __hookMethod(String.prototype, 'indexOf');
            __hookMethod(String.prototype, 'substring');
            __hookGetter(Object.prototype, 'then', undefined);
            __hookGetter(Promise.prototype, 'constructor', Promise);
            __run(window.__S[0]);
            __run(window.__S[1]);
            __run(window.__S[2]);
            __run(window.__S[3]);
            await new Promise(function(r) { setTimeout(r, 50); });
            window.__restore.forEach(function(f) { f(); });
            return {
                hits: window.__hits,
                reach: window.__reach,
                routes_left: V.getRouteRules().length,
                network_left: V.getNetworkLog().length,
                results: window.__calls.map(function(c) { return c.id + '=' + c.result; }),
            };
            ",
        )],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = result(&results, 0);
    // The ops really ran (so the hooks were exercised on the privileged paths)...
    assert_eq!(r["routes_left"], 0, "{r}");
    assert_eq!(r["network_left"], 0, "{r}");
    let settled = r["results"].to_string();
    for id in ["r4-1", "r4-2", "r4-3", "r4-4"] {
        assert!(settled.contains(id), "{id} never settled: {settled}");
    }
    assert!(
        r["hits"].as_u64().unwrap_or(0) > 0,
        "hooks never fired: {r}"
    );
    // ...yet nothing a hook could reach carries the key or is an agent-only op.
    let key = agent_key();
    let reach: Vec<String> = serde_json::from_value(r["reach"].clone()).unwrap();
    let leaked: Vec<&String> = reach.iter().filter(|s| s.contains(key)).collect();
    assert!(
        leaked.is_empty(),
        "agent key reachable from page hooks ({} of {} reached items):\n{}",
        leaked.len(),
        reach.len(),
        leaked
            .iter()
            .map(|s| s.chars().take(160).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    );
    for op_body in [
        "routeRules = routeRules.filter",
        "networkLog.splice(i, 1)",
        "dialogAutoResponses[type]",
        "key === AGENT_KEY",
    ] {
        assert!(
            !reach.iter().any(|s| s.contains(op_body)),
            "agent-only function reachable from page hooks: {op_body}"
        );
    }
}

// ── R4-JS3: logged fetch / XHR target is the one the request actually uses ───

#[test]
#[allow(clippy::too_many_lines)]
fn r4_js3_network_log_records_the_real_request_target() {
    let mut d = def(
        None,
        vec![
            case(
                "fetch: a non-Request object is requested (and logged) as String(input)",
                r"
                var V = window.__VICTAURI__;
                var requested = [];
                // Record what the underlying fetch is really asked for.
                var fake = { url: 'http://ipc.localhost/quit_app', method: 'POST',
                             toString: function() { return 'http://elsewhere.test/'; } };
                await fetch(fake);
                var log = V.getNetworkLog();
                return { url: log[0].url, method: log[0].method,
                         ipc: V.getIpcLog().map(function(e) { return e.command; }) };
                ",
            ),
            case(
                "fetch: a stateful toString cannot make the log differ from the request",
                r"
                var n = 0;
                var seen = null;
                var real = window.fetch;
                var fake = { toString: function() { return (n++ === 0) ? 'http://ipc.localhost/quit_app' : 'http://elsewhere.test/'; } };
                await fetch(fake);
                var log = window.__VICTAURI__.getNetworkLog();
                return { url: log[0].url, conversions: n,
                         ipc: window.__VICTAURI__.getIpcLog().length };
                ",
            ),
            case(
                "fetch: a replaced window.String does not change the logged URL",
                r"
                window.String = function() { return 'http://ipc.localhost/forged'; };
                await fetch({ toString: function() { return 'http://elsewhere.test/a'; } });
                var log = window.__VICTAURI__.getNetworkLog();
                return { url: log[0].url, ipc: window.__VICTAURI__.getIpcLog().length };
                ",
            ),
            case(
                "xhr: a planted __victauri_net / fake url object cannot forge the log",
                r"
                var V = window.__VICTAURI__;
                V.addRoute({ pattern: 'http', action: 'block' }); // never really send
                // (No window.String replacement here: jsdom's own XHR uses it internally.)
                var x = new XMLHttpRequest();
                x.open('GET', { toString: function() { return 'http://elsewhere.test/x'; } });
                try { x.__victauri_net = { method: 'POST', url: 'http://ipc.localhost/quit_app' }; } catch (e) {}
                x.send();
                var log = V.getNetworkLog();
                return { n: log.length, url: log[0] && log[0].url, method: log[0] && log[0].method,
                         ipc: V.getIpcLog().length };
                ",
            ),
            case(
                "xhr: a send() on a never-opened request logs nothing",
                r"
                var x = new XMLHttpRequest();
                try { x.__victauri_net = { method: 'POST', url: 'http://ipc.localhost/quit_app' }; } catch (e) {}
                try { x.send(); } catch (e) {}
                return { n: window.__VICTAURI__.getNetworkLog().length };
                ",
            ),
        ],
    );
    let Some(results) = run_tests(&d) else {
        return;
    };
    assert_all_pass(&results);
    let r0 = result(&results, 0);
    assert_eq!(r0["url"], "http://elsewhere.test/", "{r0}");
    assert_eq!(r0["method"], "GET", "{r0}");
    assert_eq!(r0["ipc"], serde_json::json!([]), "{r0}");
    let r1 = result(&results, 1);
    assert_eq!(r1["conversions"], 1, "input converted exactly once: {r1}");
    assert_eq!(r1["ipc"], 1, "{r1}");
    assert_eq!(r1["url"], "http://ipc.localhost/quit_app", "{r1}");
    let r2 = result(&results, 2);
    assert_eq!(r2["url"], "http://elsewhere.test/a", "{r2}");
    assert_eq!(r2["ipc"], 0, "{r2}");
    let r3 = result(&results, 3);
    assert_eq!(r3["n"], 1, "{r3}");
    assert_eq!(r3["url"], "http://elsewhere.test/x", "{r3}");
    assert_eq!(r3["method"], "GET", "{r3}");
    assert_eq!(r3["ipc"], 0, "{r3}");
    assert_eq!(result(&results, 4)["n"], 0);

    // Genuine `Request` objects (Node's fetch classes, exposed before the bridge loads).
    d = def(
        None,
        vec![
            case(
                "fetch: a Request is logged from its real url/method, not shadowing props",
                r"
                var req = new Request('http://ipc.localhost/real_cmd', { method: 'PUT' });
                Object.defineProperty(req, 'url', { value: 'http://ipc.localhost/quit_app' });
                Object.defineProperty(req, 'method', { value: 'DELETE' });
                await fetch(req);
                var log = window.__VICTAURI__.getNetworkLog();
                return { url: log[0].url, method: log[0].method,
                         ipc: window.__VICTAURI__.getIpcLog().map(function(e) { return e.command; }) };
                ",
            ),
            case(
                "fetch: init.method overrides a Request's method, as fetch does",
                r"
                await fetch(new Request('http://x.test/'), { method: 'PATCH' });
                return window.__VICTAURI__.getNetworkLog()[0].method;
                ",
            ),
        ],
    );
    d.node_web_api = true;
    let Some(results) = run_tests(&d) else {
        return;
    };
    assert_all_pass(&results);
    let q = result(&results, 0);
    assert_eq!(q["url"], "http://ipc.localhost/real_cmd", "{q}");
    assert_eq!(q["method"], "PUT", "{q}");
    assert_eq!(q["ipc"], serde_json::json!(["real_cmd"]), "{q}");
    assert_eq!(result(&results, 1), "PATCH");
}
