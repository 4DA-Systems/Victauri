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
    // `send` converts its argument first: one that throws starts nothing and uses up no rule, and
    // one that calls `send` itself makes the outer call throw, as on a real request.
    let body_conversion = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'mock', times:1}}); \
         var x = new XMLHttpRequest(), threw = ''; x.open('POST', '{MOCK}'); \
         try {{ x.send({{toString: function() {{ throw new RangeError('no'); }}}}); }} catch (e) {{ threw = e.name; }} \
         T(threw === 'RangeError', threw); T(B.getRouteRules()[0].triggered === 0, J(B.getRouteRules())); \
         T(B.getNetworkLog().length === 0, J(B.getNetworkLog())); \
         threw = ''; \
         try {{ x.send({{toString: function() {{ x.send(); return 'b'; }}}}); }} catch (e) {{ threw = e.name; }} \
         T(threw === 'InvalidStateError', threw); await sleep(30); \
         var log = B.getNetworkLog(); T(log.length === 1 && log[0].status === 200, J(log)); T(x.responseText === 'mock');"
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
        ("body_conversion", "<div></div>", &body_conversion),
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
        // Ambiguity in a subgroup of a repeated group: nothing about the group itself shows it.
        r"(x(a|a)y)+",
        // An exact count is that many characters: `a{2}` and `aa` are the same text.
        r"(?:a{2}b|aab)+$",
        r"(a{4}b|a{2}a{2}b)+$",
        r"(?:a{2,2}c|aac)+$",
        r"(?:v(?:\d{2}|\d\d)/)+$",
        // Two sets that share one character, named by neither: U+1680 is `\s`.
        r"(?:\s|[Ā-ᜀ])+$",
        r"((a|a)y)+",
        r"(x(a+|a+)y)+",
        r"(x(a+a+)?y)+$",
        r"(x(a+a+|b)y)+$",
        r"(/(\w+|\w+)/)+$",
        r"(/([a-z]+|[a-z0-9]+)/)+$",
        r"(([ab]{2}|[ab]))+?$",
        r"(x(|)y)+",
        r"(a|ab|b)+",
        r"(&?\w+=[^&]*)+",
        // Escapes that are not well formed take nothing after them.
        r"\x(a+)+$",
        r"\xg(a+)+$",
        r"\u(a+)+$",
        r"\u12(a+)+$",
        r"\u{(a+)+$",
        r"\c(x)(a+)+$",
        r"\k<(a+)+$",
        // Characters the pattern names by escape, and a legacy octal escape.
        r"(\u0100+)+$",
        r"(\u4e2d+\u4e2d*)+$",
        r"(\xe9+\xe9*)+$",
        r"([\u0100-\u0200]+)+$",
        r"([\u0100-\u0200]+[\u0150-\u0300]+)+",
        r"([^\u0000-\u4e2d]+[^\u0000-\u4e2d]+)+",
        r"(\012+)+",
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
        // Each takes every text one way: neighbours that share characters but not the seam.
        r"([a-z0-9]+(-[a-z0-9]+)*\.)+[a-z]{2,}$",
        r"(/\w+(-\w+)*)+$",
        r"(/[a-z]+(\.[a-z]+)?)+$",
        r"(\d+(\.\d+)?,)+$",
        // Branches that start alike and part after a character neither can repeat.
        r"(api|app)+",
        r"(foo|fab)+",
        r"(ab|ac)+",
        r"(/api|/app)+",
        r"(user|users)+",
        r"(a|ab)+",
        r"(?:\w{2})+",
        r"(a{2}b)+",
        // Escapes that are well formed are one character.
        r"(\x41b)+",
        r"(\u0041b)+",
        r"(\u0100\u0101+)+",
        r"(?<n>a)\k<n>",
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
            // A modifier changes what an atom takes. Not in every engine: there it is invalid.
            "inline_modifiers",
            "<div></div>",
            "['(?i:(a+A+)+)$', '(a+(?i:A)+)+$'].forEach(function(p) { \
               T(!AGENT.addRoute({pattern:p, match_type:'regex', action:'block'}).ok, p); }); \
             var works = true; try { new RegExp('(?i:a)'); } catch (e) { works = false; } \
             if (works) T(AGENT.addRoute({pattern:'((?i:a)b+)+', match_type:'regex', action:'block'}).ok, 'harmless');",
        ),
        (
            "a_pattern_too_large_to_analyse_is_refused_quickly",
            "<div></div>",
            "var p = '(' + Array.from({length: 3000}, function() { return 'a'; }).join('|') + ')+'; var t = Date.now(); \
             T(!AGENT.addRoute({pattern:p, match_type:'regex', action:'block'}).ok, 'allowed'); \
             T(Date.now() - t < 1500, 'took ' + (Date.now() - t));",
        ),
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

// ── Animation events during a scrub ──────────────────────────────────────────

/// A paused CSS animation, as far as the bridge reads one.
const ANIMATED: &str = r"
    window.fakeAnimation = function(el) { return {
        effect: { target: el, getComputedTiming: function() { return { endTime: 1000 }; } },
        animationName: 'k', transitionProperty: 'opacity',
        currentTime: 500, playState: 'running', ready: Promise.resolve(),
        pause: function() { this.playState = 'paused'; }, play: function() { this.playState = 'running'; },
        finish: function() { this.playState = 'finished'; },
    }; };
    window.animate = function(sel) { var el = $(sel); var a = fakeAnimation(el);
        el.getAnimations = function() { return [a]; }; return a; };
    // The event a scrubbed animation fires; `extra` overrides what it is about.
    window.fire = function(el, type, extra) {
        var props = type.indexOf('transition') === 0 ? { propertyName: 'opacity', pseudoElement: '' } : { animationName: 'k', pseudoElement: '' };
        for (var k in (extra || {})) props[k] = extra[k];
        __vtestTrustedEvent(el, type, props);
    };
    window.frames = function(n) { return new Promise(function(done) {
        (function step(left) { if (!left) done(); else requestAnimationFrame(function() { step(left - 1); }); })(n); }); };
    window.heard = [];
    ['animationstart','animationend','animationiteration','transitionend'].forEach(function(t) {
        window.addEventListener(t, function(e) { heard.push(t + ':' + e.target.id); });
        document.addEventListener(t, function(e) { heard.push('doc-' + t + ':' + e.target.id); }, true);
    });
";

#[test]
fn a_scrub_keeps_animation_events_from_the_page() {
    let during = format!(
        "{ANIMATED} animate('#a'); var p = await AGENT.scrubPrepare('#a'); T(p.prepared, J(p)); \
         ['animationstart','animationend','animationiteration','transitionend'].forEach(function(t) {{ fire($('#a'), t); }}); \
         T(heard.length === 0, 'reached the page: ' + J(heard));"
    );
    let other_elements = format!(
        "{ANIMATED} animate('#a'); await AGENT.scrubPrepare('#a'); \
         fire($('#b'), 'animationend'); \
         T(J(heard) === '[\"doc-animationend:b\",\"animationend:b\"]', J(heard));"
    );
    let synthetic_from_the_page = format!(
        "{ANIMATED} animate('#a'); await AGENT.scrubPrepare('#a'); \
         $('#a').dispatchEvent(new Event('animationend', {{bubbles:true}})); \
         T(J(heard) === '[\"doc-animationend:a\",\"animationend:a\"]', J(heard));"
    );
    let after_restore = format!(
        "{ANIMATED} animate('#a'); await AGENT.scrubPrepare('#a'); AGENT.scrubRestore(true); \
         fire($('#a'), 'animationend'); T(heard.length === 0, 'just after restore: ' + J(heard)); \
         await frames(3); \
         fire($('#a'), 'animationend'); \
         T(J(heard) === '[\"doc-animationend:a\",\"animationend:a\"]', 'after two frames: ' + J(heard));"
    );
    let no_scrub = format!(
        "{ANIMATED} fire($('#a'), 'animationstart'); \
         T(J(heard) === '[\"doc-animationstart:a\",\"animationstart:a\"]', J(heard));"
    );
    let a_second_scrub_stays_quiet = format!(
        "{ANIMATED} animate('#a'); await AGENT.scrubPrepare('#a'); AGENT.scrubRestore(true); \
         await AGENT.scrubPrepare('#a'); await frames(3); \
         fire($('#a'), 'animationend'); \
         T(heard.length === 0, 'the first restore ended the second scrub: ' + J(heard));"
    );
    let replaced_scrub_is_released = format!(
        "{ANIMATED} animate('#a'); animate('#b'); await AGENT.scrubPrepare('#a'); await AGENT.scrubPrepare('#b'); \
         await frames(3); fire($('#a'), 'animationend'); \
         T(J(heard) === '[\"doc-animationend:a\",\"animationend:a\"]', 'a is not being scrubbed any more: ' + J(heard)); \
         heard.length = 0; fire($('#b'), 'animationend'); T(heard.length === 0, J(heard));"
    );
    let inside_a_shadow_root = format!(
        "{ANIMATED} var host = $('#a'), sr = host.attachShadow({{mode:'open'}}); sr.innerHTML = '<i id=in></i>'; \
         var inner = sr.getElementById('in'), a = fakeAnimation(inner), got = 0; inner.getAnimations = function() {{ return [a]; }}; \
         inner.addEventListener('animationend', function() {{ got++; }}); \
         document.getAnimations = function() {{ return [a]; }}; \
         await AGENT.scrubPrepare(null); fire(inner, 'animationend'); \
         T(got === 0, 'a listener inside the shadow tree heard it'); \
         AGENT.scrubRestore(true); await frames(3); fire(inner, 'animationend'); T(got === 1, 'after: ' + got);"
    );
    let only_what_the_scrub_causes = format!(
        "{ANIMATED} animate('#a'); await AGENT.scrubPrepare('#a'); \
         fire($('#a'), 'animationend', {{animationName:'other'}}); \
         T(J(heard) === '[\"doc-animationend:a\",\"animationend:a\"]', 'another animation: ' + J(heard)); heard.length = 0; \
         fire($('#a'), 'animationend', {{pseudoElement:'::after'}}); \
         T(heard.length === 2, 'a pseudo-element: ' + J(heard)); heard.length = 0; \
         fire($('#a'), 'transitionend', {{propertyName:'width'}}); \
         T(heard.length === 2, 'another transition: ' + J(heard)); heard.length = 0; \
         fire($('#a'), 'transitionend'); T(heard.length === 0, 'the held transition: ' + J(heard));"
    );
    let a_natural_end_is_heard = format!(
        "{ANIMATED} var a = animate('#a'); await AGENT.scrubPrepare('#a'); AGENT.scrubRestore(true); \
         a.playState = 'finished'; fire($('#a'), 'animationend'); \
         T(heard.length === 2, 'ended by itself in the window: ' + J(heard)); heard.length = 0; \
         fire($('#a'), 'animationstart'); T(heard.length === 0, 'not an end: ' + J(heard));"
    );
    let finishing_it_again_is_not_an_end = format!(
        "{ANIMATED} var a = animate('#a'); a.playState = 'finished'; await AGENT.scrubPrepare('#a'); AGENT.scrubRestore(true); \
         T(a.playState === 'finished', a.playState); fire($('#a'), 'animationend'); \
         T(heard.length === 0, 'it had already ended, for the page: ' + J(heard));"
    );
    let a_seek_is_not_an_end = format!(
        "{ANIMATED} var a = animate('#a'); await AGENT.scrubPrepare('#a'); await AGENT.scrubSeek(1); \
         T(a.playState === 'paused', a.playState); fire($('#a'), 'animationend'); T(heard.length === 0, J(heard));"
    );
    let a_cancel_by_the_page_is_heard = format!(
        "{ANIMATED} var a = animate('#a'); await AGENT.scrubPrepare('#a'); a.playState = 'idle'; \
         var got = 0; $('#a').addEventListener('animationcancel', function() {{ got++; }}); fire($('#a'), 'animationcancel'); T(got === 1, 'cancel');"
    );
    let replaced_scrub_is_restored = format!(
        "{ANIMATED} var a = animate('#a'), b = animate('#b'); await AGENT.scrubPrepare('#a'); await AGENT.scrubSeek(0.2); \
         T(a.playState === 'paused' && a.currentTime !== 500, 'held'); \
         await AGENT.scrubPrepare('#b'); \
         T(a.playState === 'running', 'the old target stayed ' + a.playState); T(a.currentTime === 500, 'time ' + a.currentTime);"
    );
    let replaced_on_the_same_target_keeps_the_first_state = format!(
        "{ANIMATED} var a = animate('#a'); await AGENT.scrubPrepare('#a'); await AGENT.scrubSeek(0.2); \
         await AGENT.scrubPrepare('#a'); AGENT.scrubRestore(true); \
         T(a.playState === 'running', a.playState); T(a.currentTime === 500, 'time ' + a.currentTime);"
    );
    let a_failed_prepare_leaves_the_scrub = format!(
        "{ANIMATED} var a = animate('#a'); await AGENT.scrubPrepare('#a'); $('#b').getAnimations = function() {{ return []; }}; \
         var r = await AGENT.scrubPrepare('#b'); T(r.error, J(r)); T(a.playState === 'paused', a.playState); \
         var s = await AGENT.scrubSeek(0.5); T(!s.error, J(s));"
    );
    let finish_stays_quiet_for_onfinish = format!(
        "{ANIMATED} var a = animate('#a'); a.playState = 'finished'; var h = function() {{}}; a.onfinish = h; \
         a.finish = function() {{ this.playState = 'finished'; window.seenAtFinish = this.onfinish; }}; \
         await AGENT.scrubPrepare('#a'); AGENT.scrubRestore(true); \
         T(window.seenAtFinish === null, 'onfinish was set when it finished again'); \
         await frames(3); T(a.onfinish === h, 'the handler is back');"
    );
    let two_holds_share_a_shadow_root = format!(
        "{ANIMATED} var host = $('#a'), sr = host.attachShadow({{mode:'open'}}); sr.innerHTML = '<i id=in></i>'; \
         var inner = sr.getElementById('in'), a = fakeAnimation(inner), got = 0; inner.getAnimations = function() {{ return [a]; }}; \
         inner.addEventListener('animationend', function() {{ got++; }}); \
         document.getAnimations = function() {{ return [a]; }}; \
         await AGENT.scrubPrepare(null); AGENT.scrubRestore(true); await AGENT.scrubPrepare(null); \
         await frames(3); fire(inner, 'animationend'); \
         T(got === 0, 'the first release ended the second hold'); \
         AGENT.scrubRestore(true); await frames(3); fire(inner, 'animationend'); T(got === 1, 'after: ' + got);"
    );
    let html = "<div id=a></div><div id=b></div>";
    check(&[
        (
            "only_what_the_scrub_causes",
            html,
            &only_what_the_scrub_causes,
        ),
        ("a_natural_end_is_heard", html, &a_natural_end_is_heard),
        (
            "finishing_it_again_is_not_an_end",
            html,
            &finishing_it_again_is_not_an_end,
        ),
        ("a_seek_is_not_an_end", html, &a_seek_is_not_an_end),
        (
            "a_cancel_by_the_page_is_heard",
            html,
            &a_cancel_by_the_page_is_heard,
        ),
        (
            "replaced_scrub_is_restored",
            html,
            &replaced_scrub_is_restored,
        ),
        (
            "replaced_on_the_same_target",
            html,
            &replaced_on_the_same_target_keeps_the_first_state,
        ),
        (
            "a_failed_prepare_leaves_the_scrub",
            html,
            &a_failed_prepare_leaves_the_scrub,
        ),
        (
            "finish_stays_quiet_for_onfinish",
            html,
            &finish_stays_quiet_for_onfinish,
        ),
        (
            "two_holds_share_a_shadow_root",
            html,
            &two_holds_share_a_shadow_root,
        ),
        ("during", html, &during),
        ("other_elements_keep_theirs", html, &other_elements),
        ("synthetic_events_pass", html, &synthetic_from_the_page),
        ("two_frames_after_restore", html, &after_restore),
        ("no_scrub_no_effect", html, &no_scrub),
        ("second_scrub", html, &a_second_scrub_stays_quiet),
        ("replaced_scrub", html, &replaced_scrub_is_released),
        ("shadow_root", html, &inside_a_shadow_root),
    ]);
}

// ── XHR `fulfill`, against what a real server showed ────────────────────────

#[test]
fn a_fulfilled_xhr_matches_a_real_request_in_the_details() {
    let loadend_after_abort_in_load = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'x'}}); \
         var x = new XMLHttpRequest(), seen = [], at = null; x.addEventListener('load', function() {{ x.abort(); }}); record(x, seen); \
         x.addEventListener('loadend', function(e) {{ at = e.loaded; }}); \
         x.open('GET', '{MOCK}'); seen.length = 0; x.send(); await sleep(30); \
         T(J(seen.slice(-2)) === '[\"load\",\"loadend\"]', J(seen)); T(x.readyState === 0, 'state ' + x.readyState); T(at === 0, 'loaded ' + at);"
    );
    let loadend_after_open_in_load = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'x'}}); \
         var x = new XMLHttpRequest(), seen = [], n = 0; x.addEventListener('load', function() {{ if (n++ === 0) x.open('GET', '{MOCK}'); }}); record(x, seen); \
         x.open('GET', '{MOCK}'); seen.length = 0; x.send(); await sleep(30); \
         T(J(seen.slice(-2)) === '[\"load\",\"loadend\"]', J(seen)); T(x.readyState === 1, 'state ' + x.readyState);"
    );
    let loadend_in_the_sync_path = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'x'}}); \
         var x = new XMLHttpRequest(), seen = []; x.addEventListener('load', function() {{ x.abort(); }}); record(x, seen); \
         x.open('GET', '{MOCK}', false); seen.length = 0; x.send(); \
         T(J(seen.slice(-2)) === '[\"load\",\"loadend\"]', J(seen));"
    );
    let upload_totals = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'x'}}); \
         var run = async function(body) {{ var x = new XMLHttpRequest(), t = null; x.upload.addEventListener('load', function(e) {{ t = [e.loaded, e.total, e.lengthComputable]; }}); \
           x.open('POST', '{MOCK}'); x.send(body); await sleep(20); return J(t); }}; \
         T(await run('h\\u00e9llo') === '[6,6,true]', 'bytes of a string'); \
         T(await run(new Blob(['abcdef'])) === '[6,6,true]', 'a blob'); \
         T(await run(new Uint8Array(9)) === '[9,9,true]', 'a view'); \
         T(await run(new ArrayBuffer(4)) === '[4,4,true]', 'a buffer');"
    );
    let get_in_lower_case = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'x'}}); \
         var x = new XMLHttpRequest(), n = 0; x.upload.addEventListener('loadstart', function() {{ n++; }}); \
         x.open('get', '{MOCK}'); x.send('body'); await sleep(20); T(n === 0, 'a GET has no upload');"
    );
    let upload_ends_with_the_request = |how: &str, then: &str, want: &str| {
        format!(
            "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'x', delay_ms:120}}); \
             var x = new XMLHttpRequest(), seen = []; record(x, seen); \
             ['abort','timeout','loadend'].forEach(function(t) {{ x.upload.addEventListener(t, function() {{ seen.push('u' + t); }}); }}); \
             x.open('POST', '{MOCK}'); {how} x.send('abc'); seen.length = 0; {then} \
             T(J(seen) === '{want}', J(seen));"
        )
    };
    let upload_abort = upload_ends_with_the_request(
        "",
        "x.abort();",
        "[\"rs4\",\"uabort\",\"uloadend\",\"abort\",\"loadend\"]",
    );
    let upload_timeout = upload_ends_with_the_request(
        "x.timeout = 20;",
        "await sleep(60);",
        "[\"rs4\",\"utimeout\",\"uloadend\",\"timeout\",\"loadend\"]",
    );
    let timeout_after_send = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'x', delay_ms:200}}); \
         var x = new XMLHttpRequest(), seen = []; record(x, seen); x.open('GET', '{MOCK}'); x.send(); seen.length = 0; \
         x.timeout = 30; await sleep(90); \
         T(J(seen) === '[\"rs4\",\"timeout\",\"loadend\"]', J(seen)); T(x.status === 0, 'status ' + x.status); \
         await sleep(160); T(seen.length === 3, 'answered as well: ' + J(seen)); T(x.timeout === 30, 'read back');"
    );
    let timeout_after_send_already_past = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'x', delay_ms:200}}); \
         var x = new XMLHttpRequest(), seen = []; record(x, seen); x.open('GET', '{MOCK}'); x.send(); seen.length = 0; \
         await sleep(50); x.timeout = 10; await sleep(30); T(seen.indexOf('timeout') !== -1, J(seen));"
    );
    let timeout_after_send_too_late_changes_nothing = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'x', delay_ms:30}}); \
         var x = new XMLHttpRequest(), seen = []; record(x, seen); x.open('GET', '{MOCK}'); x.send(); seen.length = 0; \
         x.timeout = 5000; await sleep(70); T(seen.indexOf('load') !== -1, J(seen));"
    );
    let blob_type = |extra: &str, ct: &str, want: &str| {
        format!(
            "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'x', content_type:'{ct}'}}); \
             var x = new XMLHttpRequest(); x.open('GET', '{MOCK}'); x.responseType = 'blob'; {extra} x.send(); await sleep(20); \
             T(x.response.type === '{want}', x.response.type);"
        )
    };
    let blob_override = blob_type(
        "x.overrideMimeType('Text/X-Foo');",
        "application/json",
        "text/x-foo",
    );
    let blob_normalised = blob_type("", "Text/Plain; Charset=UTF-8", "text/plain");
    let bom_text = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'\\ufeffx', content_type:'text/plain'}}); \
         var x = new XMLHttpRequest(); x.open('GET', '{MOCK}'); x.send(); await sleep(20); \
         T(x.responseText === 'x', 'length ' + x.responseText.length); T(x.response === 'x', 'response');"
    );
    let bom_json = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'\\ufeff{{\"a\":1}}'}}); \
         var x = new XMLHttpRequest(); x.open('GET', '{MOCK}'); x.responseType = 'json'; x.send(); await sleep(20); \
         T(J(x.response) === '{{\"a\":1}}', J(x.response));"
    );
    let bom_stays_in_bytes = format!(
        "AGENT.addRoute({{pattern:'/m', action:'fulfill', body:'\\ufeffx', content_type:'text/plain'}}); \
         var x = new XMLHttpRequest(); x.open('GET', '{MOCK}'); x.responseType = 'arraybuffer'; x.send(); await sleep(20); \
         T(x.response.byteLength === 4, 'length ' + x.response.byteLength);"
    );
    check(&[
        (
            "loadend_after_abort_in_load",
            "<div></div>",
            &loadend_after_abort_in_load,
        ),
        (
            "loadend_after_open_in_load",
            "<div></div>",
            &loadend_after_open_in_load,
        ),
        (
            "loadend_in_the_sync_path",
            "<div></div>",
            &loadend_in_the_sync_path,
        ),
        ("upload_totals", "<div></div>", &upload_totals),
        ("get_in_lower_case", "<div></div>", &get_in_lower_case),
        ("upload_abort", "<div></div>", &upload_abort),
        ("upload_timeout", "<div></div>", &upload_timeout),
        ("timeout_after_send", "<div></div>", &timeout_after_send),
        (
            "timeout_after_send_already_past",
            "<div></div>",
            &timeout_after_send_already_past,
        ),
        (
            "timeout_after_send_too_late",
            "<div></div>",
            &timeout_after_send_too_late_changes_nothing,
        ),
        ("blob_override", "<div></div>", &blob_override),
        ("blob_normalised", "<div></div>", &blob_normalised),
        ("bom_text", "<div></div>", &bom_text),
        ("bom_json", "<div></div>", &bom_json),
        ("bom_stays_in_bytes", "<div></div>", &bom_stays_in_bytes),
    ]);
}

// ── The point a click would use ──────────────────────────────────────────────

/// jsdom has no layout, so each case states what the browser would answer per point.
const BOXES: &str = r"
    function box(el, x, y, w, h) { el.getBoundingClientRect = function() {
        return { left: x, top: y, right: x + w, bottom: y + h, width: w, height: h, x: x, y: y }; }; }
";

#[test]
fn a_strict_point_is_the_pixel_a_truncating_click_lands_on() {
    // SVG geometry is hit-tested to the fraction: on the stroke at 10.5, not at 10 where the click lands.
    let off_the_pixel = format!(
        "{BOXES} var t = $('#t'), o = $('#o'); box(t, 10, 10, 1, 40); \
         document.elementFromPoint = function(x) {{ return x === 10.5 ? t : o; }}; \
         var p = B.hitPoint(t, true); T(p.error, 'tested a point the click would not land on: ' + J(p));"
    );
    let on_the_pixel = format!(
        "{BOXES} var t = $('#t'), o = $('#o'); box(t, 10, 10, 1, 40); \
         document.elementFromPoint = function(x) {{ return x === 10 ? t : o; }}; \
         var p = B.hitPoint(t, true); T(p.x === 10 && p.y === 30, J(p));"
    );
    let at_two = format!(
        "{BOXES} window.devicePixelRatio = 2; var t = $('#t'), o = $('#o'); box(t, 10, 10, 0.5, 40); \
         document.elementFromPoint = function(x) {{ return x === 10 ? t : o; }}; \
         var p = B.hitPoint(t, true); T(!p.error, J(p)); T(Math.trunc(p.x * 2) === 20, J(p));"
    );
    // Every pixel survives the native side's multiply and truncation, at scales that do not divide evenly.
    let every_scale_round_trips = format!(
        "{BOXES} var t = $('#t'); document.elementFromPoint = function() {{ return t; }}; \
         [1.25, 1.5, 1.75, 1.1, 3.3333].forEach(function(d) {{ window.devicePixelRatio = d; \
           for (var px = 0; px < 900; px++) {{ box(t, px / d, 5, 0.5 / d, 2); \n             var p = B.hitPoint(t, true); T(!p.error, J(p)); \n             T(Math.trunc(p.x * d) === px, d + ' px ' + px + ': ' + p.x); \n           }} }});"
    );
    let centre_is_snapped = format!(
        "{BOXES} var t = $('#t'); box(t, 0, 0, 101, 41); document.elementFromPoint = function() {{ return t; }}; \
         var p = B.hitPoint(t, true); T(p.x === 50 && p.y === 20, J(p));"
    );
    let loose_is_exact = format!(
        "{BOXES} var t = $('#t'); box(t, 0, 0, 101, 41); document.elementFromPoint = function() {{ return t; }}; \
         var p = B.hitPoint(t, false); T(p.x === 50.5 && p.y === 20.5, J(p));"
    );
    let html = "<div id=t>t</div><div id=o>o</div>";
    check(&[
        ("off_the_pixel", html, &off_the_pixel),
        ("on_the_pixel", html, &on_the_pixel),
        ("at_two", html, &at_two),
        ("every_scale_round_trips", html, &every_scale_round_trips),
        ("centre_is_snapped", html, &centre_is_snapped),
        ("loose_is_exact", html, &loose_is_exact),
    ]);
}

#[test]
fn a_centre_outside_the_viewport_does_not_decide_it() {
    // Half off the left edge: nothing is hit at the centre, as outside any viewport.
    let visible_part_covered = format!(
        "{BOXES} var t = $('#t'), c = $('#c'), n = 0; box(t, -100, 10, 150, 40); t.onclick = function() {{ n++; }}; \
         document.elementFromPoint = function(x) {{ return x < 0 ? null : c; }}; \
         var r = B.hitPoint(t, false); T(r.error && r.error.indexOf('covered by div#c') !== -1, J(r)); \
         T(r.error.indexOf('unknown') === -1 && r.error.indexOf('-25') === -1, r.error); \
         r = B.hitPoint(t, true); T(r.error && r.error.indexOf('covered by div#c') !== -1, J(r)); \
         var click = await B.click(B.findElements({{css:'#t', include_hidden:true}})[0].ref_id, 100); T(click.ok === false, J(click)); T(n === 0, 'clicked');"
    );
    let visible_part_free = format!(
        "{BOXES} var t = $('#t'); box(t, -100, 10, 150, 40); \
         document.elementFromPoint = function(x) {{ return x < 0 ? null : t; }}; \
         var r = B.hitPoint(t, false); T(!r.error && r.x >= 0, J(r)); r = B.hitPoint(t, true); T(!r.error && r.x >= 0, J(r));"
    );
    let a_narrow_visible_sliver = format!(
        "{BOXES} var t = $('#t'); box(t, -995, 10, 1000, 40); \
         document.elementFromPoint = function(x) {{ return x < 0 ? null : t; }}; \
         var r = B.hitPoint(t, false); T(!r.error && r.x >= 0 && r.x < 5, J(r));"
    );
    let nothing_there_at_all = format!(
        "{BOXES} var t = $('#t'); box(t, -100, 10, 150, 40); document.elementFromPoint = function() {{ return null; }}; \
         var r = B.hitPoint(t, false); T(r.error && r.error.indexOf('no visible part') !== -1, J(r)); \
         T(r.error.indexOf('unknown') === -1, r.error);"
    );
    let inside_with_a_null_hit_stays = format!(
        "{BOXES} var t = $('#t'); box(t, 10, 10, 100, 40); document.elementFromPoint = function() {{ return null; }}; \
         var r = B.hitPoint(t, false); T(r.x === 60 && r.y === 30, J(r));"
    );
    let html = "<div id=t>t</div><div id=c>c</div>";
    check(&[
        ("visible_part_covered", html, &visible_part_covered),
        ("visible_part_free", html, &visible_part_free),
        ("a_narrow_visible_sliver", html, &a_narrow_visible_sliver),
        ("nothing_there_at_all", html, &nothing_there_at_all),
        (
            "inside_with_a_null_hit_stays",
            html,
            &inside_with_a_null_hit_stays,
        ),
    ]);
}

#[test]
fn a_clipped_element_is_scrolled_and_an_overlaid_one_is_not() {
    let scrolled = |stack: &str, after: &str| {
        format!(
            "{BOXES} var t = $('#t'), f = $('#f'), n = 0; box(t, 20, 300, 40, 30); \
             t.scrollIntoView = function() {{ n++; {after} }}; \
             document.elementFromPoint = function(x, y) {{ return y === 315 ? f : t; }}; \
             document.elementsFromPoint = function() {{ return {stack}; }}; \
             var r = B.hitPoint(t, false); window.__n = n; window.__r = r;"
        )
    };
    let clipped = format!(
        "{} T(__n === 1, 'scrolled ' + __n); T(!__r.error && __r.y === 100, J(__r));",
        scrolled("[f, document.body]", "box(t, 20, 85, 40, 30);")
    );
    let overlaid = format!(
        "{} T(__n === 0, 'scrolled ' + __n); T(__r.error && __r.error.indexOf('covered by div#f') !== -1, J(__r));",
        scrolled("[f, t, document.body]", "")
    );
    let html = "<div id=t>t</div><div id=f>f</div>";
    check(&[("clipped", html, &clipped), ("overlaid", html, &overlaid)]);
}

#[test]
fn content_that_takes_no_pointer_events_inside_a_control() {
    let run = |html_case: &str| {
        format!(
            "{BOXES} var s = $('#s'), b = $('#b'); box(b, 0, 0, 100, 40); box(s, 30, 10, 40, 20); {html_case}"
        )
    };
    let in_a_button = run(
        "var n = 0; b.onclick = function() { n++; }; document.elementFromPoint = function() { return b; }; \
         var p = B.hitPoint(s, false); T(p.x === 50 && p.y === 20, J(p)); \
         var r = await B.click(B.findElements({css:'#s', include_hidden:true})[0].ref_id, 100); T(r.ok, J(r)); T(n === 1, 'n=' + n); \
         T(B.hitPoint(s, true).error, 'strict stays refused');",
    );
    let covered_button = run(
        "document.elementFromPoint = function() { return document.body; }; \
         T(B.hitPoint(s, false).error.indexOf('covered') !== -1, 'the button is what must be reachable');",
    );
    let span = "<button id=b><span id=s style='pointer-events:none'>Save</span></button>";
    let none_all_the_way = "<button id=b style='pointer-events:none'><span id=s style='pointer-events:none'>Save</span></button>";
    let not_a_control = "<div id=b><span id=s style='pointer-events:none'>Save</span></div>";
    let refused = run("document.elementFromPoint = function() { return b; }; \
         T(B.hitPoint(s, false).error.indexOf('pointer-events') !== -1, J(B.hitPoint(s, false)));");
    check(&[
        ("in_a_button", span, &in_a_button),
        ("covered_button", span, &covered_button),
        ("the_button_takes_none_either", none_all_the_way, &refused),
        ("not_a_control", not_a_control, &refused),
    ]);
}
