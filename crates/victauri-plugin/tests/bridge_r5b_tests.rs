//! Round-5b audit regressions (Low findings) for the injected JS bridge (jsdom via Node.js).
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
    BridgeCapacities, agent_op_call_js, eval_wrapper_script, init_script,
};

#[derive(Serialize)]
struct TestDef {
    bridge_script: String,
    setup_html: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    setup_js: Option<String>,
    tests: Vec<TestCase>,
}

#[derive(Serialize)]
struct TestCase {
    name: String,
    code: String,
    /// Expose Node's `Request` / `Response` / `Headers` to the page before the bridge loads.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
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
        node_web_api: false,
    }
}

fn web_case(name: &str, code: &str) -> TestCase {
    TestCase {
        node_web_api: true,
        ..case(name, code)
    }
}

fn def(setup_js: Option<String>, tests: Vec<TestCase>) -> TestDef {
    TestDef {
        bridge_script: init_script(&BridgeCapacities::default()),
        setup_html: r#"<html lang="en"><head><title>R5B</title></head><body><div id="app"></div></body></html>"#
            .to_string(),
        setup_js,
        tests,
    }
}

fn result(results: &[TestResult], i: usize) -> serde_json::Value {
    results[i].result.clone().unwrap_or_default()
}

/// Records every eval outcome delivered through a Tauri-like `invoke` stub, per id, parsed.
const DELIVERY_STUB: &str = r"
window.__delivered = {};
window.__TAURI_INTERNALS__ = { invoke: function(cmd, args) {
    var a = JSON.parse(JSON.stringify(args));
    try { window.__delivered[a.id] = JSON.parse(a.result); } catch (e) { window.__delivered[a.id] = a.result; }
    return Promise.resolve(null);
} };
window.__run = function(s) { (0, eval)(s); };
window.__sleep = function(ms) { return new Promise(function(r) { setTimeout(r, ms); }); };
";

fn scripts_js(scripts: &[(&str, String)]) -> String {
    let mut js = String::from("window.__S = {};\n");
    for (name, script) in scripts {
        js.push_str(&format!(
            "window.__S[{}] = {};\n",
            serde_json::to_string(name).unwrap(),
            serde_json::to_string(script).unwrap()
        ));
    }
    js
}

// ── R5B-SCRUBKEY1: animation scrub / sweep ops are agent-only ────────────────

/// `scrubPrepare` / `scrubSeek` / `scrubRestore` / `installSweepRecorder` / `readSweep` were on
/// the page-visible `window.__VICTAURI__`, so page script could erase or replace a sweep
/// recording (`installSweepRecorder` supersedes the agent's recorder, `readSweep(true)` clears
/// it) or drive a scrub. They now sit behind the agent key like the other agent-only ops, and
/// the exact snippets the `animation` tool sends still work.
#[test]
fn r5b_scrubkey1_animation_ops_are_agent_only() {
    let scripts = [
        (
            "arm",
            eval_wrapper_script("arm", &agent_op_call_js("installSweepRecorder(null)")),
        ),
        (
            "read",
            eval_wrapper_script("read", &agent_op_call_js("readSweep(false)")),
        ),
        (
            "prep",
            eval_wrapper_script("prep", &agent_op_call_js("scrubPrepare(null)")),
        ),
        (
            "seek",
            eval_wrapper_script("seek", &agent_op_call_js("scrubSeek(0.5)")),
        ),
        (
            "restore",
            eval_wrapper_script("restore", &agent_op_call_js("scrubRestore(true)")),
        ),
    ];
    let mut setup = String::from(DELIVERY_STUB);
    setup.push_str(&scripts_js(&scripts));
    let def = def(
        Some(setup),
        vec![case(
            "page script cannot reach the ops; the agent snippets can",
            r"
            var V = window.__VICTAURI__;
            var names = ['scrubPrepare', 'scrubSeek', 'scrubRestore', 'installSweepRecorder', 'readSweep'];
            var exposed = names.filter(function(n) { return n in V; });
            __run(__S.arm);
            await __sleep(30);
            __run(__S.read);
            __run(__S.prep);
            __run(__S.seek);
            __run(__S.restore);
            await __sleep(50);
            var d = window.__delivered;
            return {
                exposed: exposed,
                listed: typeof V.listAnimations,
                arm: d.arm && d.arm.__victauri_ok,
                read: d.read && d.read.__victauri_ok,
                prep: d.prep && d.prep.__victauri_ok,
                seek: d.seek && d.seek.__victauri_ok,
                restore: d.restore && d.restore.__victauri_ok,
            };
            ",
        )],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = result(&results, 0);
    assert_eq!(r["exposed"], serde_json::json!([]), "{r}");
    assert_eq!(
        r["listed"], "function",
        "`animation list` stays public: {r}"
    );
    assert_eq!(r["arm"]["installed"], true, "{r}");
    assert_eq!(r["read"]["armed"], true, "{r}");
    assert_eq!(r["read"]["session_count"], 0, "{r}");
    // jsdom runs no animations: prepare reports that, seek reports "not prepared".
    assert!(r["prep"]["error"].as_str().is_some(), "{r}");
    assert!(r["seek"]["error"].as_str().is_some(), "{r}");
    assert_eq!(r["restore"]["restored"], false, "{r}");
}

/// The moved ops run strict, so a page hook on a built-in they call cannot walk `.caller` to
/// an op (callable without the key) or to the injected script whose source holds the key.
#[test]
fn r5b_scrubkey1_page_hooks_never_reach_the_key_through_animation_ops() {
    let scripts = [
        (
            "arm",
            eval_wrapper_script("arm", &agent_op_call_js("installSweepRecorder('#app')")),
        ),
        (
            "read",
            eval_wrapper_script("read", &agent_op_call_js("readSweep(true)")),
        ),
        (
            "prep",
            eval_wrapper_script("prep", &agent_op_call_js("scrubPrepare('#app')")),
        ),
    ];
    let mut setup = String::from(DELIVERY_STUB);
    setup.push_str(&scripts_js(&scripts));
    setup.push_str(&format!(
        "window.__KEY = {};\n",
        serde_json::to_string(victauri_plugin::js_bridge::agent_key()).unwrap()
    ));
    let def = def(
        Some(setup),
        vec![case(
            "no hook sees a caller",
            r"
            var reach = [], calls = 0;
            function hook(obj, name) {
                var orig = obj[name];
                obj[name] = function h() {
                    calls++;
                    try {
                        for (var c = h.caller, n = 0; c && n < 20; c = c.caller, n++) {
                            reach.push(name + ':' + (('' + c).indexOf(window.__KEY) !== -1 ? 'KEY' : 'fn'));
                        }
                    } catch (e) {}
                    return orig.apply(this, arguments);
                };
            }
            hook(document, 'querySelector');
            hook(window, 'requestAnimationFrame');
            hook(Array.prototype, 'map');
            hook(Array.prototype, 'filter');
            hook(Promise, 'all');
            window.getComputedStyle = (function(o) { return function gcs(el) {
                try { if (gcs.caller) reach.push('gcs:fn'); } catch (e) {}
                return o.call(window, el);
            }; })(window.getComputedStyle);
            __run(__S.arm);
            await __sleep(60);
            __run(__S.read);
            __run(__S.prep);
            await __sleep(60);
            return { reach: reach, calls: calls, read: window.__delivered.read && window.__delivered.read.__victauri_ok };
            ",
        )],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = result(&results, 0);
    assert_eq!(r["reach"], serde_json::json!([]), "{r}");
    assert_eq!(r["read"]["armed"], true, "the ops really ran: {r}");
    assert!(
        r["calls"].as_u64().unwrap_or(0) > 0,
        "the hooks were exercised: {r}"
    );
}

// ── R5B-ROUTEURL1: a rule hits the same request however the app spells it ─────

/// A route pattern matched only the URL *as the app passed it*: `fetch('/api/x')` exposed the
/// relative string, a `Request` or `URL` object the absolute URL, so an `exact` (or anchored
/// regex) rule hit one form and silently missed the other. Rules now match the absolute URL,
/// the same-origin path, and the raw string.
#[test]
fn r5b_routeurl1_rules_match_relative_and_absolute_forms() {
    let def = def(
        None,
        vec![web_case(
            "exact / regex / glob rules against string, URL and Request forms, fetch and XHR",
            r"
            var V = window.__VICTAURI__;
            V.addRoute({ pattern: '/api/rel', match_type: 'exact', action: 'block' });
            V.addRoute({ pattern: 'http://localhost/api/abs', match_type: 'exact', action: 'block' });
            V.addRoute({ pattern: '^/api/rx', match_type: 'regex', action: 'block' });
            V.addRoute({ pattern: 'http://localhost/api/gl*', match_type: 'glob', action: 'block' });
            async function blocked(input) {
                try { await fetch(input); return false; }
                catch (e) { return /blocked by route/.test(e.message); }
            }
            var abs = function(p) { return 'http://localhost' + p; };
            var out = {};
            var paths = ['/api/rel', '/api/abs', '/api/rx', '/api/glob'];
            for (var i = 0; i < paths.length; i++) {
                var p = paths[i];
                out[p] = [
                    await blocked(p),
                    await blocked(abs(p)),
                    await blocked(new URL(p, location.href)),
                    await blocked(new Request(abs(p))),
                ];
            }
            // An unrelated path is untouched in every form.
            out.other = [await blocked('/api/other'), await blocked(new Request(abs('/api/other')))];
            // XHR, relative and absolute.
            function xhrBlocked(u) {
                return new Promise(function(res) {
                    var x = new XMLHttpRequest();
                    x.open('GET', u);
                    var done = function() { res(V.getNetworkLog(null, 1)[0].status === 'blocked'); };
                    x.addEventListener('error', done);
                    x.addEventListener('loadend', done);
                    x.send();
                });
            }
            out.xhr = [await xhrBlocked('/api/abs'), await xhrBlocked(abs('/api/rel'))];
            return out;
            ",
        )],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = result(&results, 0);
    for p in ["/api/rel", "/api/abs", "/api/rx", "/api/glob"] {
        assert_eq!(
            r[p],
            serde_json::json!([true, true, true, true]),
            "{p} must be blocked in every form: {r}"
        );
    }
    assert_eq!(r["other"], serde_json::json!([false, false]), "{r}");
    assert_eq!(r["xhr"], serde_json::json!([true, true]), "{r}");
}

// ── R5B-XHR1: a reused XHR gets one log entry per request, listeners once ────

/// `send()` attached five fresh listeners to the XHR every time, each closing over THAT send's
/// log entry, so a reused XHR (open/send again on the same object) piled up listeners and every
/// later request rewrote the earlier entries' status and duration. A re-`open()` while a
/// request was in flight left its entry 'pending' forever (wedging `network_idle`), and a
/// delayed send then fired into the next request. A blocked XHR never fired `loadend`.
#[test]
fn r5b_xhr1_reused_xhr_keeps_one_entry_per_request() {
    let def = def(
        None,
        vec![case(
            "three requests on one XHR, a re-open mid-flight, a blocked request",
            r"
            var V = window.__VICTAURI__;
            function sleep(ms) { return new Promise(function(r) { setTimeout(r, ms); }); }
            function loadend(x, ms) {
                return new Promise(function(r) {
                    x.addEventListener('loadend', function() { r(true); }, { once: true });
                    setTimeout(function() { r(false); }, ms || 2000);
                });
            }
            var x = new XMLHttpRequest();
            x.open('GET', '/r5b/one'); var p = loadend(x); x.send(); await p;
            var first = V.getNetworkLog('/r5b/', 10)[0];
            await sleep(80);
            x.open('GET', '/r5b/two'); p = loadend(x); x.send(); await p;
            await sleep(80);
            x.open('GET', '/r5b/three'); p = loadend(x); x.send(); await p;
            var log = V.getNetworkLog('/r5b/', 10);

            // Re-open while a (route-delayed) request is still pending.
            V.addRoute({ pattern: '/r5c/slow', action: 'delay', delay_ms: 150 });
            var y = new XMLHttpRequest();
            y.open('GET', '/r5c/slow'); y.send();
            y.open('GET', '/r5c/after'); p = loadend(y); y.send(); await p;
            await sleep(300);
            var log2 = V.getNetworkLog('/r5c/', 10);

            // A blocked request ends like a failed one: error, then loadend.
            V.addRoute({ pattern: '/r5d/blocked', action: 'block' });
            var z = new XMLHttpRequest();
            var events = [];
            z.addEventListener('error', function() { events.push('error'); });
            z.open('GET', '/r5d/blocked'); p = loadend(z, 500); z.send();
            var ended = await p;

            return {
                urls: log.map(function(e) { return e.url; }),
                ids_distinct: new Set(log.map(function(e) { return e.id; })).size,
                first_status: first.status, first_duration: first.duration_ms,
                first_after: { status: log[0].status, duration: log[0].duration_ms },
                pending: log.filter(function(e) { return e.status === 'pending'; }).length,
                reopen: log2.map(function(e) { return e.url + '=' + e.status; }),
                blocked_loadend: ended, blocked_events: events,
            };
            ",
        )],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = result(&results, 0);
    assert_eq!(
        r["urls"],
        serde_json::json!(["/r5b/one", "/r5b/two", "/r5b/three"]),
        "{r}"
    );
    assert_eq!(r["ids_distinct"], 3, "{r}");
    assert_eq!(r["pending"], 0, "{r}");
    assert_eq!(
        r["first_after"]["duration"], r["first_duration"],
        "a later request on the same XHR rewrote the first entry: {r}"
    );
    assert_eq!(r["first_after"]["status"], r["first_status"], "{r}");
    assert_eq!(
        r["reopen"],
        serde_json::json!(["/r5c/slow=aborted", "/r5c/after=error"]),
        "{r}"
    );
    assert_eq!(r["blocked_loadend"], true, "{r}");
    assert_eq!(r["blocked_events"], serde_json::json!(["error"]), "{r}");
}

// ── R5B-FETCH0: fetch() with no arguments rejects like the native one ───────

/// The interceptor converted a missing input to the string "undefined" and fetched it (a
/// relative request to `./undefined`) instead of letting native fetch reject.
#[test]
fn r5b_fetch0_fetch_without_arguments_rejects_natively() {
    let def = def(
        None,
        vec![case(
            "fetch() rejects with the native TypeError and logs nothing",
            r"
            var V = window.__VICTAURI__;
            var outcome;
            try { await fetch(); outcome = 'resolved'; }
            catch (e) { outcome = e.name + ': ' + e.message; }
            // One argument still works and is logged.
            await fetch('/r5b/one');
            return { outcome: outcome, urls: V.getNetworkLog().map(function(e) { return e.url; }) };
            ",
        )],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = result(&results, 0);
    assert!(
        r["outcome"]
            .as_str()
            .is_some_and(|o| o.starts_with("TypeError: Failed to execute 'fetch'")),
        "{r}"
    );
    assert_eq!(r["urls"], serde_json::json!(["/r5b/one"]), "{r}");
}

// ── R5B-DATAURL1: logged URLs are bounded ────────────────────────────────────

/// The network log kept every request URL verbatim, so an app loading large `data:` URLs
/// through fetch/XHR pinned up to 1000 × the URL size (1000 × 1 MB = 1 GB) in the log. Logged
/// URLs are now capped with a length marker; route matching still sees the full URL.
#[test]
fn r5b_dataurl1_logged_urls_are_capped() {
    let def = def(
        None,
        vec![case(
            "1000 fetches of a 1 MB data: URL + an XHR; a route still matches the tail",
            r"
            var V = window.__VICTAURI__;
            var big = 'data:text/plain,' + 'A'.repeat(1024 * 1024) + 'TAIL';
            V.addRoute({ pattern: 'TAIL', action: 'block' });
            var blocked = 0;
            var ps = [];
            for (var i = 0; i < 1000; i++) {
                ps.push(fetch(big).then(function() {}, function(e) {
                    if (/blocked by route/.test(e.message)) blocked++;
                }));
            }
            await Promise.all(ps);
            V._agent; // (no-op: keep the log)
            var x = new XMLHttpRequest();
            x.open('GET', 'data:text/plain,' + 'B'.repeat(200000));
            await new Promise(function(r) { x.addEventListener('loadend', r); x.send(); });
            var log = V.getNetworkLog(null, 2000);
            var lens = log.map(function(e) { return e.url.length; });
            var total = lens.reduce(function(a, b) { return a + b; }, 0);
            return {
                count: log.length, max_len: Math.max.apply(null, lens), total: total,
                sample: log[0].url.slice(-40), xhr_tail: log[log.length - 1].url.slice(-40),
                blocked: blocked,
                matches_url_len: V.getRouteMatches(1)[0].url.length,
            };
            ",
        )],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = result(&results, 0);
    assert_eq!(r["count"], 1000, "{r}");
    assert!(r["max_len"].as_u64().unwrap() <= 2200, "{r}");
    assert!(r["total"].as_u64().unwrap() <= 1000 * 2200, "{r}");
    assert!(
        r["sample"].as_str().unwrap().contains("chars]"),
        "the cut is marked: {r}"
    );
    assert!(r["xhr_tail"].as_str().unwrap().contains("chars]"), "{r}");
    assert_eq!(r["blocked"], 1000, "matching still sees the full URL: {r}");
    assert!(r["matches_url_len"].as_u64().unwrap() <= 2200, "{r}");
}

// ── R5B-PRISTINE1: captured IPC bodies and page evals use the init-time JSON ──

/// The fetch interceptor parsed captured IPC request/response bodies with the PAGE's
/// `JSON.parse`, so page script replacing it rewrote what `logs ipc` / replay / the catalog
/// report for every later call.
#[test]
fn r5b_pristine1_ipc_bodies_are_parsed_with_the_captured_json() {
    let def = def(
        None,
        vec![case(
            "a replaced JSON.parse does not forge captured args / results",
            r#"
            var V = window.__VICTAURI__;
            var realParse = JSON.parse;
            JSON.parse = function() { return { forged: true }; };
            try {
                await fetch('http://ipc.localhost/transfer', {
                    method: 'POST', body: '{"amount":10}',
                    headers: { 'x-vtest-body': '{"ok":true}' }
                });
                await new Promise(function(r) { setTimeout(r, 30); });
            } finally { JSON.parse = realParse; }
            var e = V.getIpcLog(1)[0];
            return { args: e.args, result: e.result };
            "#,
        )],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = result(&results, 0);
    assert_eq!(r["args"], serde_json::json!({"amount": 10}), "{r}");
    assert_eq!(r["result"], serde_json::json!({"ok": true}), "{r}");
}

/// The in-app `victauri_eval_js` command's script serialized the result (and built the
/// callback args) with the page's `JSON` and plain objects: a page `Object.prototype.toJSON`
/// that throws made every page-originated eval hang until its timeout, and a replaced
/// `JSON.stringify` forged its result. It now serializes like the agent eval path (R5-JS2).
#[test]
fn r5b_pristine1_page_eval_is_robust_to_page_json_tampering() {
    use victauri_plugin::js_bridge::page_eval_script;
    let scripts = [
        ("obj", page_eval_script("\"p-obj\"", "return {a: 1}")),
        ("undef", page_eval_script("\"p-undef\"", "return undefined")),
        (
            "thr",
            page_eval_script("\"p-thr\"", "throw new Error('boom')"),
        ),
        (
            "circ",
            page_eval_script("\"p-circ\"", "var o = {}; o.o = o; return o"),
        ),
        ("forge", page_eval_script("\"p-forge\"", "return {b: 2}")),
        (
            "tojson",
            page_eval_script("\"p-tojson\"", "return {c: [3]}"),
        ),
    ];
    // A Tauri-like invoke: serializes the args with the native JSON.stringify (captured before
    // the test tampers with the global), which still consults a planted `toJSON`.
    let mut setup = String::from(
        r"
        var __JS = JSON.stringify, __JP = JSON.parse;
        window.__got = {};
        window.__TAURI_INTERNALS__ = { invoke: function(cmd, args) {
            var a = __JP(__JS(args));
            window.__got[a.id] = a.result;
            return Promise.resolve(null);
        } };
        ",
    );
    setup.push_str(&scripts_js(&scripts));
    let def = def(
        Some(setup),
        vec![case(
            "results arrive despite a throwing toJSON / replaced stringify",
            r#"
            function sleep(ms) { return new Promise(function(r) { setTimeout(r, ms); }); }
            Object.defineProperty(Object.prototype, 'toJSON', {
                configurable: true, value: function() { throw new Error('page toJSON'); }
            });
            try {
                (0, eval)(__S.obj);
                (0, eval)(__S.undef);
                (0, eval)(__S.thr);
                (0, eval)(__S.circ);
                await sleep(30);
            } finally { delete Object.prototype.toJSON; }
            var realStringify = JSON.stringify;
            JSON.stringify = function() { return '"forged"'; };
            try { (0, eval)(__S.forge); await sleep(30); }
            finally { JSON.stringify = realStringify; }
            // A forging (non-throwing) universal toJSON is ignored, as for agent evals.
            Object.defineProperty(Object.prototype, 'toJSON', {
                configurable: true, value: function() { return 'forged'; }
            });
            Array.prototype.toJSON = function() { return 'forged'; };
            try { (0, eval)(__S.tojson); await sleep(30); }
            finally { delete Object.prototype.toJSON; delete Array.prototype.toJSON; }
            return window.__got;
            "#,
        )],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = result(&results, 0);
    // A throwing universal toJSON: reported (like an agent eval's "unserializable"), never
    // left to hang until the timeout as before.
    for id in ["p-obj", "p-circ"] {
        assert!(
            r[id]
                .as_str()
                .is_some_and(|s| s.starts_with("{\"__error\":")),
            "{id}: {r}"
        );
    }
    assert_eq!(r["p-undef"], "null", "{r}");
    assert_eq!(r["p-thr"], "{\"__error\":\"boom\"}", "{r}");
    assert_eq!(r["p-forge"], "{\"b\":2}", "{r}");
    assert_eq!(r["p-tojson"], "{\"c\":[3]}", "{r}");
}
