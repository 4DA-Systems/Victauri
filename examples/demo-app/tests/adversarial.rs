//! Adversarial, multi-step E2E tests against the real running demo-app.
//!
//! This is the bug-catching counterpart to `integration.rs` (which is mostly
//! happy-path). Every test here is a regression guard for a real bug we fixed,
//! an error-path assertion (verify it *fails correctly*), a multi-step workflow
//! that checks an invariant (not just "it worked"), or a stress/concurrency
//! check. Uses the REST API for unambiguous `{result}` vs `{error}` assertions
//! (the MCP client flattens the `isError` flag).
//!
//! Gated by `VICTAURI_E2E=1`; requires the demo-app running. Run:
//! ```sh
//! # build + launch demo-app, then:
//! VICTAURI_E2E=1 cargo test -p demo-app --test adversarial
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};
use victauri_test::is_e2e;

// ── Harness ─────────────────────────────────────────────────────────────────

async fn base_url() -> String {
    let c = victauri_test::connect()
        .await
        .expect("connect to demo-app (set VICTAURI_E2E=1 and launch the app)");
    c.base_url().to_string()
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

/// Convenience: the `{result}` payload, panicking if the call returned an error.
fn result(v: &Value) -> &Value {
    assert!(
        v.get("error").is_none(),
        "expected success, got error: {}",
        v["error"]
    );
    &v["result"]
}

/// Convenience: the error string, panicking if the call succeeded.
fn error(v: &Value) -> String {
    v.get("error")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected an error, got success: {}", v["result"]))
        .to_string()
}

async fn eval(base: &str, code: &str) -> Value {
    call(base, "eval_js", json!({ "code": code })).await
}

/// Resolve a demo-app element ref by its data-testid.
async fn ref_by_testid(base: &str, test_id: &str) -> String {
    let r = call(base, "find_elements", json!({ "test_id": test_id })).await;
    let arr = result(&r).as_array().expect("find_elements array");
    assert!(!arr.is_empty(), "no element with data-testid='{test_id}'");
    arr[0]["ref_id"].as_str().expect("ref_id").to_string()
}

macro_rules! adv {
    ($name:ident, $base:ident, $body:block) => {
        #[tokio::test]
        async fn $name() {
            if !is_e2e() {
                return;
            }
            let $base = base_url().await;
            $body
        }
    };
}

// ── A. eval_js correctness regressions (the critical silent-corruption class) ─

adv!(
    eval_multistatement_member_assign_returns_correct_value,
    base,
    {
        // The exact shape of the CRITICAL bug: code starting with a member
        // assignment, then `return`. Used to silently return the assignment value.
        let r = eval(
            &base,
            "window.__adv = {a: 5}; window.__adv.b = window.__adv.a * 2; return window.__adv.b",
        )
        .await;
        assert_eq!(
            result(&r),
            &json!(10),
            "multi-statement eval returned wrong value"
        );
    }
);

adv!(eval_do_then_return_pattern, base, {
    // localStorage.setItem(...); return localStorage.getItem(...) — used to → undefined
    let r = eval(
        &base,
        "localStorage.setItem('adv_k', 'adv_v'); return localStorage.getItem('adv_k')",
    )
    .await;
    assert_eq!(result(&r), &json!("adv_v"));
});

adv!(eval_bare_expression_auto_returns, base, {
    let r = eval(&base, "1 + 2 + 3").await;
    assert_eq!(result(&r), &json!(6));
    // A string literal containing a semicolon must NOT be split into statements.
    let r2 = eval(&base, "'a;b;c'").await;
    assert_eq!(result(&r2), &json!("a;b;c"));
});

adv!(eval_deep_nesting_no_envelope_leak, base, {
    // Past serde_json's 128 recursion limit. Must return the value, NOT the raw
    // `{"__victauri_ok":...}` envelope, and must NOT crash the host.
    let r = eval(
        &base,
        "let o={}; let c=o; for(let i=0;i<300;i++){c.n={};c=c.n} return o",
    )
    .await;
    let s = serde_json::to_string(result(&r)).unwrap();
    assert!(
        !s.contains("__victauri_ok"),
        "envelope leaked: {}",
        &s[..s.len().min(60)]
    );
    // Host still alive afterwards (no stack-overflow crash).
    let alive = eval(&base, "return 7*7").await;
    assert_eq!(
        result(&alive),
        &json!(49),
        "host died after deep-nesting eval"
    );
});

adv!(eval_unicode_roundtrip_exact_codepoints, base, {
    let r = eval(
        &base,
        "const s=String.fromCharCode(26085,26412,35486); return [...s].map(c=>c.charCodeAt(0))",
    )
    .await;
    assert_eq!(result(&r), &json!([26085, 26412, 35486]));
});

adv!(eval_undefined_null_and_exception, base, {
    assert_eq!(result(&eval(&base, "undefined").await), &json!("undefined"));
    assert_eq!(result(&eval(&base, "null").await), &json!(null));
    let e = error(&eval(&base, "throw new Error('adv_boom')").await);
    assert!(e.contains("adv_boom"), "exception not surfaced: {e}");
});

adv!(eval_oversized_output_is_capped, base, {
    let e = error(&eval(&base, "return 'x'.repeat(6*1024*1024)").await);
    assert!(e.contains("too large"), "5MB cap not enforced: {e}");
});

// ── B. Resilience: recover cleanly after an eval timeout ────────────────────

adv!(eval_recovers_after_timeout, base, {
    // A syntax error surfaces only as the (30s) timeout. The NEXT eval must
    // succeed quickly (the bridge is alive) — proves the re-probe doesn't
    // false-fail. (Slow: ~30s.)
    let e = error(&eval(&base, "return 1 +").await);
    assert!(e.contains("timed out"), "expected timeout, got: {e}");
    let r = eval(&base, "return 42").await;
    assert_eq!(
        result(&r),
        &json!(42),
        "eval did not recover after a timeout"
    );
});

// ── C. query_db: read-only enforcement (verify it FAILS correctly) ──────────

adv!(query_db_blocks_all_writes, base, {
    for sql in [
        "INSERT INTO x VALUES (1)",
        "UPDATE x SET a=1",
        "DELETE FROM x",
        "DROP TABLE x",
        "PRAGMA journal_mode = WAL",
    ] {
        let e = error(&call(&base, "query_db", json!({ "query": sql })).await);
        assert!(
            e.contains("read-only") || e.contains("PRAGMA writes"),
            "write not blocked for `{sql}`: {e}"
        );
    }
    // Stacked queries blocked.
    let e = error(&call(&base, "query_db", json!({"query":"SELECT 1; DROP TABLE x"})).await);
    assert!(e.contains("stacked"), "stacked query not blocked: {e}");
    // A read still works.
    let ok = call(&base, "query_db", json!({"query":"SELECT 1 AS x"})).await;
    assert_eq!(result(&ok)["rows"][0]["x"], json!(1));
});

// ── D. Filesystem + selector + command error paths ─────────────────────────

adv!(read_app_file_blocks_traversal, base, {
    let r = call(
        &base,
        "read_app_file",
        json!({"path": "../../../../../../etc/passwd"}),
    )
    .await;
    assert!(r.get("error").is_some(), "path traversal not blocked: {r}");
});

adv!(find_elements_rejects_invalid_selector, base, {
    let e = error(&call(&base, "find_elements", json!({"css": ">>>nope"})).await);
    assert!(e.contains("selector"), "invalid selector not rejected: {e}");
});

adv!(interact_on_stale_ref_fails_cleanly, base, {
    let r = call(
        &base,
        "interact",
        json!({"action":"click","ref_id":"e99999999"}),
    )
    .await;
    // Either a tool error or an ok:false with a ref-not-found hint — never a hang/panic.
    let body = serde_json::to_string(&r).unwrap();
    assert!(
        body.contains("not found") || body.contains("ok\":false") || r.get("error").is_some(),
        "stale ref not handled cleanly: {body}"
    );
});

adv!(invoke_nonexistent_command_surfaces_error, base, {
    let r = call(
        &base,
        "invoke_command",
        json!({"command":"adv_does_not_exist"}),
    )
    .await;
    assert!(
        r.get("error").is_some(),
        "nonexistent command did not error: {r}"
    );
});

adv!(window_edge_cases_error, base, {
    let ghost = call(
        &base,
        "window",
        json!({"action":"get_state","label":"adv_ghost"}),
    )
    .await;
    assert!(
        ghost.get("error").is_some(),
        "ghost window did not error: {ghost}"
    );
    let zero = call(
        &base,
        "window",
        json!({"action":"resize","label":"main","width":0,"height":0}),
    )
    .await;
    assert!(
        zero.get("error").is_some(),
        "0x0 resize did not error: {zero}"
    );
});

// ── E. Multi-step: cross-boundary divergence is actually DETECTED ───────────

adv!(verify_state_detects_injected_divergence, base, {
    // The point of verify_state is catching drift — so prove it actually FAILS
    // on a real mismatch, not just that it passes when things agree (that would
    // be the same false-confidence trap as happy-path testing).
    // verify_state compares the frontend_expr result against the WHOLE
    // backend_state value, so the shapes must match.
    // 1. Agreement → passes.
    let agree = result(
        &call(
            &base,
            "verify_state",
            json!({"frontend_expr": "'same'", "backend_state": "same"}),
        )
        .await,
    )
    .clone();
    assert_eq!(
        agree["passed"],
        json!(true),
        "verify_state false-negative on agreement: {agree}"
    );
    // 2. Injected mismatch → MUST be detected.
    let r = call(
        &base,
        "verify_state",
        json!({"frontend_expr": "'__adv_wrong__'", "backend_state": "__adv_right__"}),
    )
    .await;
    let res = result(&r);
    assert_eq!(
        res["passed"],
        json!(false),
        "verify_state FAILED TO DETECT an injected divergence (false confidence!): {res}"
    );
    assert!(
        res["divergences"].as_array().map_or(0, std::vec::Vec::len) > 0,
        "no divergences reported despite a mismatch: {res}"
    );
});

adv!(counter_ui_and_backend_stay_consistent, base, {
    // Multi-step: read DOM counter → click increment → DOM increments → backend matches.
    let read_dom = || async {
        eval(
            &base,
            "return parseInt(document.querySelector('[data-testid=counter-value]').textContent,10)",
        )
        .await
    };
    let before = result(&read_dom().await).as_i64().expect("counter int");
    let inc = ref_by_testid(&base, "increment-btn").await;
    let click = call(&base, "interact", json!({"action":"click","ref_id":inc})).await;
    assert!(
        click.get("error").is_none(),
        "increment click failed: {click}"
    );
    // Give the UI a tick to update.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let after = result(&read_dom().await).as_i64().expect("counter int");
    assert_eq!(
        after,
        before + 1,
        "DOM counter did not increment via UI click"
    );
    // IPC integrity stays healthy after the interaction.
    let health = result(&call(&base, "check_ipc_integrity", json!({})).await).clone();
    assert_eq!(
        health["healthy"],
        json!(true),
        "IPC unhealthy after interaction: {health}"
    );
});

// ── F. Phase 1: route — network interception (block / fulfill / delay) ──────

adv!(route_block_fulfill_delay_and_clear, base, {
    call(&base, "route", json!({"action":"clear_all"})).await;
    // fulfill
    let add = call(
        &base,
        "route",
        json!({"action":"add","pattern":"adv-mock.example","behavior":"fulfill","status":418,"body":{"adv":true}}),
    )
    .await;
    assert_eq!(result(&add)["ok"], json!(true));
    let got = eval(
        &base,
        "return fetch('https://adv-mock.example/x').then(r=>r.json().then(j=>({s:r.status,a:j.adv})))",
    )
    .await;
    assert_eq!(result(&got)["s"], json!(418), "fulfill status wrong");
    assert_eq!(result(&got)["a"], json!(true), "fulfill body wrong");
    // block
    call(
        &base,
        "route",
        json!({"action":"add","pattern":"adv-block.example","behavior":"block"}),
    )
    .await;
    let blocked = eval(
        &base,
        "return fetch('https://adv-block.example/x').then(()=>'NO').catch(e=>'BLOCKED:'+e.message)",
    )
    .await;
    assert!(
        result(&blocked).as_str().unwrap_or("").contains("BLOCKED"),
        "block did not reject: {}",
        result(&blocked)
    );
    // delay
    call(
        &base,
        "route",
        json!({"action":"add","pattern":"adv-delay.example","behavior":"delay","delay_ms":900}),
    )
    .await;
    let elapsed = eval(
        &base,
        "const t=Date.now(); return fetch('https://adv-delay.example/x').then(()=>Date.now()-t).catch(()=>Date.now()-t)",
    )
    .await;
    assert!(
        result(&elapsed).as_i64().unwrap_or(0) >= 900,
        "delay not applied: {}",
        result(&elapsed)
    );
    // clear_all → mock no longer applies (real network fails for the fake host).
    let cleared = call(&base, "route", json!({"action":"clear_all"})).await;
    assert!(result(&cleared)["removed"].as_i64().unwrap_or(0) >= 3);
    let real = eval(
        &base,
        "return fetch('https://adv-mock.example/x').then(r=>'status:'+r.status).catch(()=>'real-network')",
    )
    .await;
    assert_eq!(result(&real), &json!("real-network"), "route not cleared");
});

// ── G. Phase 3: same-origin iframe traversal (demo-app has none — inject one) ─

adv!(iframe_traversal_and_interaction, base, {
    eval(
        &base,
        "(function(){var o=document.getElementById('advf');if(o)o.remove();var f=document.createElement('iframe');f.id='advf';document.body.appendChild(f);var d=f.contentDocument;var b=d.createElement('button');b.id='advfb';b.textContent='AdvFrame';b.onclick=function(){b.textContent='FCLICK'};d.body.appendChild(b);return 'ok';})()",
    )
    .await;
    let snap = call(&base, "dom_snapshot", json!({"format":"compact"})).await;
    let tree = result(&snap)["tree"].as_str().unwrap_or("");
    assert!(
        tree.contains("iframe content"),
        "snapshot did not descend into iframe"
    );
    let r = call(&base, "find_elements", json!({"css":"#advfb"})).await;
    let frame_ref = result(&r)[0]["ref_id"]
        .as_str()
        .expect("frame element ref")
        .to_string();
    call(
        &base,
        "interact",
        json!({"action":"click","ref_id":frame_ref}),
    )
    .await;
    let txt = eval(
        &base,
        "return document.getElementById('advf').contentDocument.getElementById('advfb').textContent",
    )
    .await;
    assert_eq!(
        result(&txt),
        &json!("FCLICK"),
        "click did not reach the frame element"
    );
});

// ── H. Phase 4: trace — screencast bundle ───────────────────────────────────

adv!(trace_captures_frames_and_events, base, {
    assert_eq!(
        result(
            &call(
                &base,
                "trace",
                json!({"action":"start","interval_ms":200,"max_frames":10,"with_events":true})
            )
            .await
        )["started"],
        json!(true)
    );
    // Do something while tracing.
    let inc = ref_by_testid(&base, "increment-btn").await;
    call(&base, "interact", json!({"action":"click","ref_id":inc})).await;
    tokio::time::sleep(std::time::Duration::from_millis(900)).await;
    let stop = call(&base, "trace", json!({"action":"stop"})).await;
    assert!(
        result(&stop)["frame_count"].as_i64().unwrap_or(0) >= 1,
        "no frames captured: {}",
        result(&stop)
    );
    // REST collapses single-content results, so `frames` is a single image
    // object when one frame is returned, or an array when several are.
    let frames = call(&base, "trace", json!({"action":"frames","limit":3})).await;
    let res = result(&frames);
    let first = if res.is_array() { &res[0] } else { res };
    assert_eq!(
        first["mimeType"],
        json!("image/png"),
        "trace frame is not a PNG: {first}"
    );
    assert!(
        first["data"].as_str().unwrap_or("").starts_with("iVBORw0K"),
        "trace frame data is not PNG-encoded"
    );
});

// ── I. Phase 2: trusted input — correct on Windows, graceful elsewhere ───────

adv!(trusted_input_platform_behaviour, base, {
    // Arm a keydown isTrusted recorder on a fresh input and focus it.
    eval(
        &base,
        "(function(){var o=document.getElementById('advti');if(o)o.remove();var i=document.createElement('input');i.id='advti';window.__advk=[];i.addEventListener('keydown',e=>window.__advk.push(e.isTrusted));document.body.insertBefore(i,document.body.firstChild);return 'ok';})()",
    )
    .await;
    let r = ref_by_testid_or_css(&base, "advti").await;
    call(
        &base,
        "window",
        json!({"action":"manage","label":"main","manage_action":"focus"}),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let res = call(
        &base,
        "input",
        json!({"action":"type_text","ref_id":r,"text":"Hi","trusted":true}),
    )
    .await;
    if cfg!(windows) {
        // Either real trusted input (isTrusted:true) or a clear error — never a hang/empty.
        assert!(
            res.get("error").is_some() || res["result"]["trusted"] == json!(true),
            "trusted input returned an unexpected shape on Windows: {res}"
        );
    } else {
        // Non-Windows must degrade gracefully with a clear "not implemented" error.
        let e = error(&res);
        assert!(
            e.contains("not implemented") || e.to_lowercase().contains("native"),
            "expected graceful trusted-input fallback on this platform, got: {e}"
        );
    }
});

async fn ref_by_testid_or_css(base: &str, id: &str) -> String {
    let r = call(base, "find_elements", json!({ "css": format!("#{id}") })).await;
    result(&r)[0]["ref_id"]
        .as_str()
        .expect("ref_id")
        .to_string()
}

// ── J. Stress: rapid + concurrent calls keep the app consistent + alive ─────

adv!(rapid_fire_evals_stay_consistent, base, {
    for i in 0..40 {
        let r = eval(&base, &format!("return {i} * 2")).await;
        assert_eq!(
            result(&r),
            &json!(i * 2),
            "rapid eval {i} returned wrong value"
        );
    }
    // App still healthy.
    let alive = eval(&base, "return 'alive'").await;
    assert_eq!(result(&alive), &json!("alive"));
});

adv!(concurrent_tool_calls_are_isolated, base, {
    // Fire many independent calls concurrently against separate connections and
    // confirm each returns its own correct, isolated result.
    let mut handles = Vec::new();
    for i in 0..16 {
        let b = base.clone();
        handles.push(tokio::spawn(async move {
            let r = call(
                &b,
                "eval_js",
                json!({ "code": format!("return {i} + {i}") }),
            )
            .await;
            (i, r["result"].as_i64().unwrap_or(-1))
        }));
    }
    for h in handles {
        let (i, got) = h.await.unwrap();
        assert_eq!(
            got,
            i + i,
            "concurrent eval {i} got wrong/cross-talk result {got}"
        );
    }
});
