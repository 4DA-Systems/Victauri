//! Limits the bridge used to document instead of closing: a mocked XHR, a route regex that can
//! hang the page, and the page's own animation events during a scrub.
//!
//! Same harness as `bridge_tests.rs`: the real bridge under jsdom. jsdom has no Web Animations, so
//! the scrub cases stub `getAnimations` with the fields the bridge reads from a real `Animation`,
//! and fire trusted events the way a browser does. Real engines: `examples/gauntlet-app`.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use victauri_plugin::js_bridge::{BridgeCapacities, agent_key, init_script};

#[derive(Serialize)]
struct TestDef {
    bridge_script: String,
    setup_html: String,
    setup_js: String,
    tests: Vec<TestCase>,
}

#[derive(Serialize)]
struct TestCase {
    name: String,
    code: String,
    setup_html: String,
    node_web_api: bool,
}

#[derive(Deserialize, Debug)]
struct TestResult {
    name: String,
    passed: bool,
    error: Option<String>,
}

fn runner_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("bridge_tests")
}

const SETUP: &str = r"
window.T = function(c, m) { if (!c) throw new Error(m || 'assertion failed'); };
window.B = window.__VICTAURI__;
window.J = function(v) { return JSON.stringify(v); };
window.$ = function(s) { return document.querySelector(s); };
window.sleep = function(ms) { return new Promise(function(r) { setTimeout(r, ms); }); };
window.record = function(x, seen) {
    ['readystatechange','progress','load','error','abort','timeout','loadend','loadstart'].forEach(function(t) {
        x.addEventListener(t, function() { seen.push(t === 'readystatechange' ? 'rs' + x.readyState : t); });
    });
};
";

fn check(cases: &[(&str, &str, &str)]) {
    if !runner_dir().join("node_modules").join("jsdom").exists() {
        assert!(
            std::env::var_os("CI").is_none()
                && std::env::var_os("VICTAURI_REQUIRE_JSDOM").is_none(),
            "jsdom is not installed: `npm ci` in crates/victauri-plugin/tests/bridge_tests/"
        );
        eprintln!("SKIP: jsdom not installed (run `npm ci` in tests/bridge_tests/)");
        return;
    }
    let def = TestDef {
        bridge_script: init_script(&BridgeCapacities::default()),
        setup_html: String::new(),
        setup_js: SETUP.to_string(),
        tests: cases
            .iter()
            .map(|(name, html, code)| TestCase {
                name: (*name).to_string(),
                code: code.replace(
                    "AGENT",
                    &format!("window.__VICTAURI__._agent('{}')", agent_key()),
                ),
                setup_html: format!(
                    "<html lang=\"en\"><head><title>t</title></head><body>{html}</body></html>"
                ),
                node_web_api: true,
            })
            .collect(),
    };
    let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
    tmp.write_all(serde_json::to_string(&def).expect("json").as_bytes())
        .expect("write");
    tmp.flush().expect("flush");
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
                "no results.\nstdout: {stdout}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
    let results: Vec<TestResult> =
        serde_json::from_str(&line["VICTAURI_RESULTS:".len()..]).expect("results json");
    assert_eq!(results.len(), cases.len());
    let failures: Vec<String> = results
        .iter()
        .filter(|r| !r.passed)
        .map(|r| format!("  {}: {}", r.name, r.error.as_deref().unwrap_or("?")))
        .collect();
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}

// ── XHR `fulfill` ────────────────────────────────────────────────────────────

/// Port 9 refuses or swallows everything: a request that reached the network never loads.
const MOCK: &str = "http://127.0.0.1:9/m";

#[test]
fn a_fulfilled_xhr_reports_the_mock() {
    let surface = format!(
        "var a = AGENT.addRoute({{pattern:'/m', action:'fulfill', status:201, status_text:'Made', \
           headers:{{'X-One':'1','Set-Cookie':'a=b'}}, body:'hello', content_type:'text/plain'}}); \
         T(a.ok, J(a)); T(!('xhr_note' in a), J(a)); \
         var x = new XMLHttpRequest(), seen = []; record(x, seen); \
         var onload = 0, onrs = 0; x.onload = function() {{ onload++; }}; x.onreadystatechange = function() {{ onrs++; }}; \
         x.open('GET', '{MOCK}#frag'); seen.length = 0; onrs = 0; \
         T(x.responseURL === '', 'before send ' + x.responseURL); \
         x.send(); \
         T(x.readyState === 1, 'send returned at ' + x.readyState); T(x.status === 0, 'status before headers'); \
         T(x.getResponseHeader('x-one') === null, 'header before headers'); T(x.getAllResponseHeaders() === ''); \
         T(J(seen) === '[\"loadstart\"]', 'async events ran inside send: ' + J(seen)); \
         await sleep(30); \
         T(J(seen) === '[\"loadstart\",\"rs2\",\"rs3\",\"progress\",\"rs4\",\"load\",\"loadend\"]', J(seen)); \
         T(onload === 1 && onrs === 3, 'on* handlers ' + onload + ' ' + onrs); \
         T(x.readyState === 4 && x.status === 201 && x.statusText === 'Made', x.status + x.statusText); \
         T(x.responseText === 'hello' && x.response === 'hello', x.responseText); \
         T(x.responseURL === '{MOCK}', x.responseURL); \
         T(x.getResponseHeader('X-ONE') === '1', 'case-insensitive'); \
         T(x.getResponseHeader('content-type') === 'text/plain', 'content type'); \
         T(x.getResponseHeader('set-cookie') === null, 'set-cookie is never readable'); \
         T(x.getResponseHeader('nope') === null, 'absent'); \
         T(x.getAllResponseHeaders() === 'content-type: text/plain\\r\\nx-one: 1\\r\\n', J(x.getAllResponseHeaders())); \
         var e = B.getNetworkLog()[0]; \
         T(e.status === 201 && e.status_text === 'Made' && e.mocked === true && e.route_rule === a.id, J(e)); \
         T(!('fulfill_not_applied' in e), J(e)); T(typeof e.duration_ms === 'number', J(e)); \
         var m = B.getRouteMatches(); T(m.length === 1 && !('applied' in m[0]), J(m));"
    );
    let progress = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'h\\u00e9'}}); \
         var x = new XMLHttpRequest(), p = []; x.addEventListener('progress', function(e) {{ p.push([e.lengthComputable, e.loaded, e.total]); }}); \
         x.addEventListener('load', function(e) {{ p.push([e.loaded, e.total]); }}); \
         x.open('GET', '{MOCK}'); x.send(); await sleep(30); \
         T(J(p) === '[[true,3,3],[3,3]]', 'bytes, not characters: ' + J(p));"
    );
    let log_is_final_at_load = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'x'}}); var at = null; \
         var x = new XMLHttpRequest(); x.onload = function() {{ at = B.getNetworkLog()[0].status; }}; \
         x.open('GET', '{MOCK}'); x.send(); await sleep(30); T(at === 200, 'log said ' + at);"
    );
    let relative = "AGENT.addRoute({pattern:'/rel', action:'fulfill', body:'x'}); \
         var x = new XMLHttpRequest(); x.open('GET', '/rel?a=1'); x.send(); await sleep(30); \
         T(x.responseURL === 'http://localhost/rel?a=1', x.responseURL);";
    check(&[
        ("surface", "<div></div>", &surface),
        ("progress_counts_bytes", "<div></div>", &progress),
        ("log_is_final_at_load", "<div></div>", &log_is_final_at_load),
        ("relative_url_is_resolved", "<div></div>", relative),
    ]);
}

#[test]
fn a_fulfilled_xhr_honours_response_type() {
    let with = |rt: &str, body: &str, ct: &str, then: &str| {
        format!(
            "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:{body}, content_type:'{ct}'}}); \
             var x = new XMLHttpRequest(); x.open('GET', '{MOCK}'); x.responseType = '{rt}'; var mid = []; \
             x.onreadystatechange = function() {{ if (x.readyState < 4) mid.push(x.response); }}; \
             x.send(); await sleep(30); {then}"
        )
    };
    let text = with(
        "text",
        "'abc'",
        "text/plain",
        "T(x.response === 'abc', J(x.response)); T(x.responseText === 'abc');",
    );
    let json = with(
        "json",
        "'{\"a\":[1]}'",
        "application/json",
        "T(J(x.response) === '{\"a\":[1]}', J(x.response)); T(x.response === x.response, 'one object'); \
         T(mid[0] === null && mid[1] === null, 'no object before done ' + J(mid)); \
         var threw = null; try { x.responseText; } catch (e) { threw = e.name; } T(threw === 'InvalidStateError', 'responseText ' + threw);",
    );
    let bad_json = with(
        "json",
        "'{nope'",
        "application/json",
        "T(x.response === null, J(x.response)); T(x.status === 200);",
    );
    let buffer = with(
        "arraybuffer",
        "'h\\u00e9'",
        "text/plain",
        "T(Object.prototype.toString.call(x.response) === '[object ArrayBuffer]', 'type'); \
         T(J(Array.from(new Uint8Array(x.response))) === '[104,195,169]', 'utf-8'); T(x.response === x.response, 'one buffer');",
    );
    let blob = with(
        "blob",
        "'abc'",
        "text/x-thing",
        "T(x.response instanceof Blob, 'type'); T(x.response.type === 'text/x-thing', x.response.type); T(x.response.size === 3, 'size');",
    );
    let doc_html = with(
        "document",
        "'<p id=a>hi</p>'",
        "text/html",
        "T(x.response.getElementById('a').textContent === 'hi', 'parsed'); T(x.responseXML === x.response, 'responseXML is the same document');",
    );
    let doc_xml = with(
        "document",
        "'<r><i>1</i></r>'",
        "application/xml",
        "T(x.response.documentElement.nodeName === 'r', 'parsed');",
    );
    let doc_other = with(
        "document",
        "'plain'",
        "text/plain",
        "T(x.response === null, 'not a document type'); T(x.responseXML === null);",
    );
    let doc_bad_xml = with(
        "document",
        "'<r>'",
        "text/xml",
        "T(x.response === null, 'not well-formed');",
    );
    let default_xml = with(
        "",
        "'<r/>'",
        "text/xml",
        "T(x.responseXML.documentElement.nodeName === 'r', 'xml'); T(x.responseText === '<r/>');",
    );
    let default_html_is_not_parsed = with(
        "",
        "'<p>'",
        "text/html",
        "T(x.responseXML === null, 'html is parsed only for responseType document');",
    );
    let null_body = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', status:204, body:'ignored'}}); \
         var x = new XMLHttpRequest(), seen = []; record(x, seen); x.open('GET', '{MOCK}'); seen.length = 0; x.send(); await sleep(30); \
         T(x.status === 204 && x.responseText === '', x.status + J(x.responseText)); \
         T(J(seen) === '[\"loadstart\",\"rs2\",\"rs4\",\"load\",\"loadend\"]', 'no LOADING without a body: ' + J(seen));"
    );
    check(&[
        ("text", "<div></div>", &text),
        ("json", "<div></div>", &json),
        ("invalid_json_is_null", "<div></div>", &bad_json),
        ("arraybuffer", "<div></div>", &buffer),
        ("blob", "<div></div>", &blob),
        ("document_html", "<div></div>", &doc_html),
        ("document_xml", "<div></div>", &doc_xml),
        ("document_of_another_type", "<div></div>", &doc_other),
        ("document_not_well_formed", "<div></div>", &doc_bad_xml),
        ("default_type_parses_xml", "<div></div>", &default_xml),
        (
            "default_type_leaves_html",
            "<div></div>",
            &default_html_is_not_parsed,
        ),
        ("null_body_status", "<div></div>", &null_body),
    ]);
}

#[test]
fn a_fulfilled_xhr_has_the_lifecycle_of_a_request() {
    let times = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'mock', times:1}}); \
         var a = new XMLHttpRequest(); a.open('GET', '{MOCK}'); a.send(); await sleep(30); T(a.responseText === 'mock', 'first'); \
         var b = new XMLHttpRequest(), seen = []; record(b, seen); b.open('GET', '{MOCK}'); b.send(); \
         b.abort(); \
         T(seen.indexOf('load') === -1, J(seen)); T(b.responseText === '', 'second is real'); \
         T(B.getRouteRules()[0].triggered === 1, J(B.getRouteRules()));"
    );
    let reopen_clears = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', status:201, body:'mock'}}); \
         var x = new XMLHttpRequest(), seen = []; record(x, seen); x.open('GET', '{MOCK}'); x.send(); await sleep(30); T(x.status === 201); \
         seen.length = 0; x.open('GET', 'http://127.0.0.1:9/other'); \
         T(J(seen) === '[\"rs1\"]', 'the state change is announced: ' + J(seen)); \
         T(x.readyState === 1 && x.status === 0 && x.responseText === '' && x.responseURL === '', 'state ' + x.readyState + x.status); \
         T(x.getAllResponseHeaders() === '', 'headers cleared'); \
         T(x.getResponseHeader('content-type') === null); \
         x.open('GET', '{MOCK}?2'); x.send(); await sleep(30); \
         T(x.status === 201 && x.responseURL === '{MOCK}?2', x.responseURL); \
         var log = B.getNetworkLog(); T(log.length === 2 && log[1].status === 201, J(log));"
    );
    let reopen_in_the_gap = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'mock', times:1}}); \
         var x = new XMLHttpRequest(), seen = []; record(x, seen); x.open('GET', '{MOCK}'); x.send(); seen.length = 0; \
         x.open('GET', 'http://127.0.0.1:9/z'); await sleep(30); \
         T(seen.indexOf('load') === -1 && seen.indexOf('rs2') === -1, J(seen)); \
         T(B.getNetworkLog()[0].status === 'aborted', J(B.getNetworkLog()));"
    );
    let abort_in_the_gap = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'mock'}}); \
         var x = new XMLHttpRequest(), seen = []; record(x, seen); x.open('GET', '{MOCK}'); x.send(); seen.length = 0; \
         x.abort(); \
         T(J(seen) === '[\"rs4\",\"abort\",\"loadend\"]', J(seen)); \
         T(x.readyState === 0 && x.status === 0 && x.responseText === '', 'state ' + x.readyState); \
         await sleep(30); T(seen.indexOf('load') === -1, 'loaded after abort: ' + J(seen)); \
         T(B.getNetworkLog()[0].status === 'aborted', J(B.getNetworkLog())); \
         var threw = null; try {{ x.send(); }} catch (e) {{ threw = e.name; }} T(threw === 'InvalidStateError', 'send after abort ' + threw); \
         x.open('GET', '{MOCK}'); x.send(); await sleep(30); T(x.responseText === 'mock', 'usable again');"
    );
    let abort_in_a_handler = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'mock'}}); \
         var x = new XMLHttpRequest(), seen = []; x.onreadystatechange = function() {{ if (x.readyState === 2) x.abort(); }}; \
         record(x, seen); x.open('GET', '{MOCK}'); x.send(); await sleep(30); \
         T(seen.indexOf('load') === -1 && seen.indexOf('rs3') === -1, J(seen)); T(seen.indexOf('abort') !== -1, J(seen));"
    );
    let abort_after_done = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'mock'}}); \
         var x = new XMLHttpRequest(), seen = []; record(x, seen); x.open('GET', '{MOCK}'); x.send(); await sleep(30); seen.length = 0; \
         x.abort(); T(seen.length === 0, 'no events ' + J(seen)); T(x.readyState === 0 && x.status === 0, 'unsent'); \
         T(B.getNetworkLog()[0].status === 200, J(B.getNetworkLog()));"
    );
    let send_twice = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'mock'}}); \
         var x = new XMLHttpRequest(); x.open('GET', '{MOCK}'); x.send(); \
         var threw = null; try {{ x.send(); }} catch (e) {{ threw = e.name; }} T(threw === 'InvalidStateError', 'in flight ' + threw); \
         await sleep(30); threw = null; try {{ x.send(); }} catch (e) {{ threw = e.name; }} T(threw === 'InvalidStateError', 'done ' + threw); \
         T(B.getNetworkLog().length === 1, J(B.getNetworkLog()));"
    );
    let delayed = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'mock', delay_ms: 60}}); \
         var x = new XMLHttpRequest(), seen = []; record(x, seen); x.open('GET', '{MOCK}'); x.send(); seen.length = 0; \
         await sleep(20); T(seen.length === 0 && x.readyState === 1, 'early ' + J(seen)); \
         T(B.getNetworkLog()[0].route_delay_ms === 60, J(B.getNetworkLog())); \
         await sleep(100); T(x.responseText === 'mock', 'after the delay'); T(seen.indexOf('load') !== -1, J(seen));"
    );
    let abort_while_delayed = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'mock', delay_ms: 40}}); \
         var x = new XMLHttpRequest(), seen = []; record(x, seen); x.open('GET', '{MOCK}'); x.send(); seen.length = 0; x.abort(); \
         await sleep(80); T(J(seen) === '[\"rs4\",\"abort\",\"loadend\"]', J(seen));"
    );
    let timeout_before_the_delay = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'mock', delay_ms: 500}}); \
         var x = new XMLHttpRequest(), seen = []; record(x, seen); x.open('GET', '{MOCK}'); x.timeout = 30; x.send(); seen.length = 0; \
         await sleep(80); \
         T(J(seen) === '[\"rs4\",\"timeout\",\"loadend\"]', J(seen)); \
         T(x.status === 0 && x.responseText === '' && x.readyState === 4, 'errored ' + x.status); \
         T(B.getNetworkLog()[0].status === 'timeout', J(B.getNetworkLog()));"
    );
    let synchronous = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', status:202, body:'now', delay_ms: 5000}}); \
         var x = new XMLHttpRequest(), seen = []; record(x, seen); x.open('GET', '{MOCK}', false); seen.length = 0; x.send(); \
         T(x.status === 202 && x.responseText === 'now', 'answered inside send: ' + x.status); \
         T(J(seen) === '[\"rs4\",\"load\",\"loadend\"]', J(seen));"
    );
    let upload = "AGENT.addRoute({pattern:'/m', action:'fulfill', body:'ok'}); \
         var x = new XMLHttpRequest(), seen = []; \
         ['loadstart','progress','load','loadend'].forEach(function(t) { x.upload.addEventListener(t, function() { seen.push('u' + t); }); }); \
         x.addEventListener('load', function() { seen.push('load'); }); \
         x.open('POST', 'http://127.0.0.1:9/m'); x.send('body'); await sleep(30); \
         T(J(seen) === '[\"uloadstart\",\"uprogress\",\"uload\",\"uloadend\",\"load\"]', J(seen));";
    let handler_error = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'ok'}}); \
         var x = new XMLHttpRequest(), done = 0; x.onreadystatechange = function() {{ throw new Error('page bug'); }}; \
         x.addEventListener('loadend', function() {{ done++; }}); x.open('GET', '{MOCK}'); x.send(); await sleep(30); \
         T(done === 1, 'a throwing handler ends nothing');"
    );
    let not_matching_is_native = "AGENT.addRoute({pattern:'/m', action:'fulfill', body:'x'}); \
         var x = new XMLHttpRequest(); T(x.readyState === 0 && x.status === 0 && x.responseText === '', 'fresh'); \
         T(x.getAllResponseHeaders() === '' && x.getResponseHeader('a') === null, 'fresh headers'); \
         var threw = null; try { Object.getOwnPropertyDescriptor(XMLHttpRequest.prototype, 'status').get.call({}); } catch (e) { threw = e.name; } \
         T(threw === 'TypeError', 'brand check ' + threw);";
    check(&[
        ("times", "<div></div>", &times),
        ("reopen_clears_the_mock", "<div></div>", &reopen_clears),
        ("reopen_in_the_gap", "<div></div>", &reopen_in_the_gap),
        ("abort_in_the_gap", "<div></div>", &abort_in_the_gap),
        ("abort_in_a_handler", "<div></div>", &abort_in_a_handler),
        ("abort_after_done", "<div></div>", &abort_after_done),
        ("send_twice", "<div></div>", &send_twice),
        ("delayed", "<div></div>", &delayed),
        ("abort_while_delayed", "<div></div>", &abort_while_delayed),
        (
            "timeout_before_the_delay",
            "<div></div>",
            &timeout_before_the_delay,
        ),
        ("synchronous", "<div></div>", &synchronous),
        ("upload_events", "<div></div>", upload),
        ("a_throwing_handler", "<div></div>", &handler_error),
        ("native_when_no_mock", "<div></div>", not_matching_is_native),
    ]);
}

// ── A route regex that can hang the page ─────────────────────────────────────

/// JavaScript has no regex timeout, and a rule's regex runs on the main thread against every
/// request URL. What repeats a group that can match the same text in more than one way is refused.
#[test]
fn a_regex_that_can_hang_the_page_is_refused_at_add() {
    let refused = [
        r"(a+)+",
        r"(a*)*",
        r"(a|aa)+",
        r"(.*)*",
        r"(\d+)*",
        r"^(a+)+$",
        r"(a+)+?",
        r"(?:a+)+",
        r"(?<n>a+)+",
        r"((a+))+",
        r"((a+)b*)+",
        r"(a+){2,}",
        r"(a+){2}",
        r"(a?a)+",
        r"(a?b?)+",
        r"(\w+\s?)+",
        r"(a|a)*",
        r"(\w|\d)+",
        r"(a|ab)+",
        r"(a|)+",
        r"(a+|b)+",
        r"(xa+|a+y)+",
        r"(x+x+y)+",
        r"(a+,a+)+",
        r"(.*a)+",
        r"(a.*)+",
        r"(/.*)+",
        r"((ab)+)+",
        r"(([a-z])+)+",
        r"(\1a+)+",
        r"(?=(a+)+)x",
        r"x|(a+)+",
    ];
    let allowed = [
        r"^/api/(users|items)/\d+$",
        r".*\.png$",
        r"^https?://",
        r"/v[0-9]+/",
        r"(a|b)+",
        r"(foo|bar)*",
        r"(ab)+",
        r"(aa)+",
        r"(a{2})+",
        r"(a+)",
        r"(a+)?",
        r"(a*)b",
        r"(/\w+)+",
        r"(/[^/]*)*",
        r"(\.[a-z]+)+$",
        r"(\w+/)+",
        r"(\w+\s)+",
        r"(a+b)+",
        r"(a+b+)+",
        r"(\s*,\s*\w+)*",
        r"(\d{1,3}\.){3}\d{1,3}",
        r"((a|b)+c)+",
        r"((a|b)c)+",
        r"(?:https?|wss?)://[^/]+/",
        r"[(]a+[)]+",
        r"\(a+\)+",
        r"a{2,}",
        r"x{",
        r"(a+)|(b+)",
    ];
    let list = |patterns: &[&str]| serde_json::to_string(patterns).expect("json");
    let refused_code = format!(
        "{}.forEach(function(p) {{ \
           var r = AGENT.addRoute({{pattern:p, match_type:'regex', action:'block'}}); \
           T(r.ok === false, 'allowed ' + p); \
           T(r.error.indexOf('can hang the page') !== -1, p + ': ' + r.error); \
         }}); T(B.getRouteRules().length === 0, 'none stored');",
        list(&refused)
    );
    let allowed_code = format!(
        "{}.forEach(function(p) {{ \
           var r = AGENT.addRoute({{pattern:p, match_type:'regex', action:'block'}}); \
           T(r.ok === true, 'refused ' + p + ': ' + r.error); \
         }});",
        list(&allowed)
    );
    check(&[
        ("refused", "<div></div>", &refused_code),
        ("allowed", "<div></div>", &allowed_code),
        (
            "other_match_types_are_not_regexes",
            "<div></div>",
            "T(AGENT.addRoute({pattern:'(a+)+', action:'block'}).ok, 'substring'); \
             T(AGENT.addRoute({pattern:'(a+)+', match_type:'glob', action:'block'}).ok, 'glob'); \
             T(AGENT.addRoute({pattern:'(a+)+', match_type:'exact', action:'block'}).ok, 'exact');",
        ),
        (
            "the_error_quotes_the_group",
            "<div></div>",
            "var r = AGENT.addRoute({pattern:'^/x/(a+)+$', match_type:'regex', action:'block'}); \
             T(r.error.indexOf('(a+)+') !== -1, r.error);",
        ),
        (
            "invalid_regex_keeps_its_own_error",
            "<div></div>",
            "var r = AGENT.addRoute({pattern:'(a+', match_type:'regex', action:'block'}); \
             T(r.error.indexOf('invalid regex pattern') === 0, r.error);",
        ),
    ]);
}
