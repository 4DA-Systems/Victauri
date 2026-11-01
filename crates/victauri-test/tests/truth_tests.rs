//! Every assertion and matcher here used to report something false. The mock answers with the
//! shapes the REAL bridge and tools produce (`js_bridge.rs`, `mcp/mod.rs`, `mcp/helpers.rs`): mock
//! fixtures with invented shapes are how `assert_accessible` could never fail for many releases.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use victauri_test::locator::Locator;
use victauri_test::{TestError, VictauriClient};

type Handler = Arc<dyn Fn(&str, &Value) -> Result<String, String> + Send + Sync>;
type Calls = Arc<Mutex<Vec<(String, Value)>>>;

async fn mock(handler: Handler) -> (u16, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let app = axum::Router::new().route(
        "/mcp",
        axum::routing::post(move |body: String| {
            let (handler, calls) = (Arc::clone(&handler), Arc::clone(&seen));
            async move {
                let v: Value = serde_json::from_str(&body).unwrap_or_default();
                let id = v.get("id").cloned();
                let reply = match v["method"].as_str().unwrap_or("") {
                    "initialize" => json!({"jsonrpc":"2.0","id":id,"result":{
                        "protocolVersion":"2025-06-18","capabilities":{},
                        "serverInfo":{"name":"mock","version":"0"}}}),
                    m if m.starts_with("notifications/") => json!({}),
                    _ => {
                        let name = v["params"]["name"].as_str().unwrap_or("").to_string();
                        let args = v["params"]["arguments"].clone();
                        calls.lock().unwrap().push((name.clone(), args.clone()));
                        match handler(&name, &args) {
                            Ok(t) => json!({"jsonrpc":"2.0","id":id,"result":{
                                "content":[{"type":"text","text":t}],"isError":false}}),
                            Err(t) => json!({"jsonrpc":"2.0","id":id,"result":{
                                "content":[{"type":"text","text":t}],"isError":true}}),
                        }
                    }
                };
                axum::Json(reply)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, calls)
}

async fn client(handler: Handler) -> (VictauriClient, Calls) {
    let (port, calls) = mock(handler).await;
    (
        VictauriClient::connect_with_token(port, None)
            .await
            .unwrap(),
        calls,
    )
}

/// One element as `findElements` returns it.
fn el(ref_id: &str, tag: &str, text: &str) -> Value {
    json!({"ref_id":ref_id,"tag":tag,"role":null,"name":null,"text":text,
           "bounds":{"x":0,"y":0,"width":1,"height":1},"visible":true,"enabled":true,"value":null})
}

fn answers_empty() -> Handler {
    Arc::new(|_, _| Ok("[]".to_string()))
}

// ── Accessibility ───────────────────────────────────────────────────────────

/// `auditAccessibility()` output, from the real bridge under jsdom.
const AUDIT_CRITICAL: &str = r#"{"violations":[{"rule":"img-alt","severity":"critical","element":"img","message":"Image missing alt text"},{"rule":"button-name","severity":"serious","element":"button","message":"Button has no accessible name"}],"warnings":[],"summary":{"critical":1,"serious":1,"moderate":0,"minor":0,"total":2}}"#;
const AUDIT_SERIOUS: &str = r#"{"violations":[{"rule":"button-name","severity":"serious","element":"button","message":"Button has no accessible name"}],"warnings":[],"summary":{"critical":0,"serious":1,"moderate":0,"minor":0,"total":1}}"#;
const AUDIT_CLEAN: &str = r#"{"violations":[],"warnings":[],"summary":{"critical":0,"serious":0,"moderate":0,"minor":0,"total":0}}"#;

fn answers(text: &'static str) -> Handler {
    Arc::new(move |_, _| Ok(text.to_string()))
}

#[tokio::test]
async fn assert_accessible_counts_the_real_violations() {
    let (mut c, _) = client(answers(AUDIT_CRITICAL)).await;
    let err = c.assert_accessible().await.unwrap_err();
    assert!(matches!(err, TestError::Assertion(_)), "{err:?}");
    assert!(err.to_string().contains('2'), "{err}");

    let (mut c, _) = client(answers(AUDIT_SERIOUS)).await;
    assert!(
        c.assert_accessible().await.is_err(),
        "strict: any violation"
    );

    let (mut c, _) = client(answers(AUDIT_CLEAN)).await;
    c.assert_accessible().await.unwrap();

    // Fails closed: an audit that does not say is not a clean audit.
    let (mut c, _) = client(answers(r#"{"summary":{"total":0}}"#)).await;
    assert!(c.assert_accessible().await.is_err());
}

#[test]
fn assert_no_a11y_violations_reads_the_real_shape() {
    let audit = |text: &str| serde_json::from_str::<Value>(text).unwrap();
    victauri_test::assert_no_a11y_violations(&audit(AUDIT_CLEAN));
    for bad in [AUDIT_CRITICAL, AUDIT_SERIOUS, r#"{"summary":{"total":0}}"#] {
        let audit = audit(bad);
        let result = std::panic::catch_unwind(|| victauri_test::assert_no_a11y_violations(&audit));
        assert!(result.is_err(), "{bad}");
    }
}

async fn smoke_a11y_check(audit: &'static str) -> victauri_test::SmokeCheckResult {
    let (mut c, _) = client(Arc::new(move |name, args| match name {
        "inspect" if args["action"] == "audit_accessibility" => Ok(audit.to_string()),
        // Skips the recording check and its 2 s wait.
        "recording" => Err("recording session already active".to_string()),
        _ => Ok("null".to_string()),
    }))
    .await;
    let report = c.smoke_test().await.unwrap();
    report
        .checks
        .into_iter()
        .find(|check| check.name == "accessibility audit")
        .unwrap()
}

#[tokio::test]
async fn smoke_fails_only_on_critical_and_reports_the_rest() {
    let critical = smoke_a11y_check(AUDIT_CRITICAL).await;
    assert!(!critical.passed, "{critical:?}");
    let serious = smoke_a11y_check(AUDIT_SERIOUS).await;
    assert!(serious.passed, "{serious:?}");
    assert!(serious.detail.contains("1 serious"), "{serious:?}");
    let clean = smoke_a11y_check(AUDIT_CLEAN).await;
    assert!(clean.passed, "{clean:?}");
    let unknown = smoke_a11y_check(r#"{"violations":[]}"#).await;
    assert!(!unknown.passed, "{unknown:?}");
}

// ── Ghost commands ──────────────────────────────────────────────────────────

#[tokio::test]
async fn no_ghost_commands_fails_on_a_confirmed_ghost_and_names_it() {
    // `build_ghost_report` output: confirmed ghosts are REMOVED from frontend_only.
    let report = json!({
        "confirmed_ghosts":[{"name":"delete_acount","error":"command delete_acount not found"}],
        "verified_handlers":3,"frontend_only":[],"post_message_only":[],"excluded_builtins":[],
        "registry_only":[],"total_frontend_commands":4,"total_registry_commands":4,
        "reliability":"high","note":"..."
    });
    let (mut c, _) = client(Arc::new(move |_, _| Ok(report.to_string()))).await;
    let r = c.verify().no_ghost_commands().run().await.unwrap();
    assert!(!r.all_passed());
    assert!(r.results[0].detail.contains("delete_acount"), "{r:?}");
}

#[tokio::test]
async fn no_ghost_commands_does_not_fail_on_an_unconfirmed_candidate() {
    // `reliability` is "high" whenever the registry has one entry; this command exists and only
    // returned a domain error, so the tool's own note says to confirm it before filing.
    let report = json!({
        "confirmed_ghosts":[], "verified_handlers":1,
        "frontend_only":[{"name":"save_thing","source":"FrontendOnly","description":null}],
        "post_message_only":[],"excluded_builtins":[],"registry_only":[],
        "total_frontend_commands":2,"total_registry_commands":1,"reliability":"high","note":"..."
    });
    let (mut c, _) = client(Arc::new(move |_, _| Ok(report.to_string()))).await;
    let r = c.verify().no_ghost_commands().run().await.unwrap();
    assert!(r.all_passed(), "{r:?}");
    assert!(r.results[0].detail.contains("save_thing"), "{r:?}");
}

// ── Console ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn no_uncaught_errors_sees_unhandled_rejections() {
    let log = json!([{"level":"error","message":"[unhandled rejection] boom","timestamp":1}]);
    let (mut c, _) = client(Arc::new(move |_, _| Ok(log.to_string()))).await;
    let err = c.assert_no_uncaught_errors().await.unwrap_err();
    assert!(err.to_string().contains("boom"), "{err}");
}

// ── Expectations: errors are not absence ────────────────────────────────────

#[tokio::test]
async fn a_failure_is_never_read_as_the_condition_being_met() {
    let (mut c, _) = client(Arc::new(|_, _| Err("bridge not responding".to_string()))).await;
    let l = Locator::test_id("banner");
    assert!(l.expect(&mut c).not().to_have_text("E").await.is_err());
    assert!(l.expect(&mut c).not().to_contain_text("E").await.is_err());
    assert!(l.expect(&mut c).not().to_have_value("x").await.is_err());
    assert!(
        l.expect(&mut c)
            .not()
            .to_have_attribute("a", "b")
            .await
            .is_err()
    );
    assert!(l.expect(&mut c).not().to_be_checked().await.is_err());
    assert!(l.expect(&mut c).not().to_be_unchecked().await.is_err());
    assert!(l.expect(&mut c).not().to_be_focused().await.is_err());
    assert!(l.expect(&mut c).not().to_have_count(5).await.is_err());
    assert!(l.expect(&mut c).to_have_count(0).await.is_err());
    assert!(l.expect(&mut c).not().to_be_visible().await.is_err());
}

#[tokio::test]
async fn an_invalid_selector_has_no_count() {
    let (mut c, _) = client(Arc::new(|_, _| {
        Err("invalid CSS selector: [bad — not a valid selector".to_string())
    }))
    .await;
    let err = Locator::css("[bad")
        .expect(&mut c)
        .to_have_count(0)
        .await
        .unwrap_err();
    assert!(matches!(err, TestError::ToolError(_)), "{err:?}");
}

#[tokio::test]
async fn a_dead_app_passes_nothing() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let app = axum::Router::new().route(
        "/mcp",
        axum::routing::post(|body: String| async move {
            let v: Value = serde_json::from_str(&body).unwrap_or_default();
            axum::Json(json!({"jsonrpc":"2.0","id":v.get("id").cloned(),"result":{
                "protocolVersion":"2025-06-18","capabilities":{},
                "serverInfo":{"name":"m","version":"0"}}}))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                rx.await.ok();
            })
            .await
            .unwrap();
    });
    let mut c = VictauriClient::connect_with_token(port, None)
        .await
        .unwrap();
    tx.send(()).unwrap();
    server.await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let l = Locator::test_id("error-banner");
    assert!(l.expect(&mut c).not().to_contain_text("E").await.is_err());
    assert!(l.expect(&mut c).to_have_count(0).await.is_err());
}

// ── Expectations: absence ───────────────────────────────────────────────────

#[tokio::test]
async fn only_hidden_and_detached_pass_on_a_missing_element() {
    let (mut c, _) = client(answers_empty()).await;
    let l = Locator::test_id("typo");
    macro_rules! short {
        () => {
            l.expect(&mut c).timeout_ms(30).poll_ms(10)
        };
    }

    short!().to_be_hidden().await.unwrap();
    short!().not().to_be_visible().await.unwrap();
    short!().to_be_detached().await.unwrap();
    short!().not().to_be_attached().await.unwrap();
    short!().to_have_count(0).await.unwrap();

    assert!(short!().to_be_disabled().await.is_err());
    assert!(short!().not().to_be_enabled().await.is_err());
    assert!(short!().not().to_have_text("x").await.is_err());
    assert!(short!().not().to_contain_text("x").await.is_err());
    assert!(short!().not().to_have_value("x").await.is_err());
    assert!(short!().not().to_have_attribute("a", "b").await.is_err());
    assert!(short!().not().to_be_checked().await.is_err());
    assert!(short!().not().to_be_unchecked().await.is_err());
    assert!(short!().not().to_be_focused().await.is_err());
}

// ── Expectations: auto-waiting ──────────────────────────────────────────────

/// A page whose element renders on the third lookup; reads answer `read`.
fn appears_on_third_poll(read: &'static str) -> Handler {
    let polls = Arc::new(Mutex::new(0u32));
    Arc::new(move |name, _| {
        if name == "find_elements" {
            let mut n = polls.lock().unwrap();
            *n += 1;
            if *n < 3 {
                return Ok("[]".to_string());
            }
            return Ok(json!([el("k3f9e1", "div", "Done")]).to_string());
        }
        Ok(read.to_string())
    })
}

#[tokio::test]
async fn every_matcher_waits_for_the_element_to_appear() {
    macro_rules! waits {
        ($read:expr, $matcher:ident ( $($arg:expr),* )) => {{
            let (mut c, _) = client(appears_on_third_poll($read)).await;
            let toast = Locator::test_id("toast");
            toast
                .expect(&mut c)
                .poll_ms(10)
                .$matcher($($arg),*)
                .await
                .unwrap_or_else(|e| panic!("{}: {e:?}", stringify!($matcher)));
        }};
    }
    waits!("\"Done\"", to_have_text("Done"));
    waits!("\"Done\"", to_contain_text("Done"));
    waits!("\"Done\"", to_have_value("Done"));
    waits!("\"Done\"", to_have_attribute("a", "Done"));
    waits!("true", to_be_checked());
    waits!("false", to_be_unchecked());
    waits!("true", to_be_focused());
}

#[tokio::test]
async fn a_ref_that_went_stale_is_not_an_empty_value() {
    // What `element_script` returns when the ref no longer resolves.
    let (mut c, _) = client(Arc::new(|name, _| {
        if name == "find_elements" {
            Ok(json!([el("k3f9e1", "div", "Done")]).to_string())
        } else {
            Ok(json!({"__victauri_missing_ref": true}).to_string())
        }
    }))
    .await;
    let l = Locator::test_id("x");
    macro_rules! short {
        () => {
            l.expect(&mut c).timeout_ms(30).poll_ms(10)
        };
    }
    assert!(short!().not().to_have_text("anything").await.is_err());
    assert!(short!().not().to_be_checked().await.is_err());
    assert!(short!().to_be_unchecked().await.is_err());
}

// ── Locator queries ─────────────────────────────────────────────────────────

fn bridge_args(calls: &Calls) -> Value {
    let calls = calls.lock().unwrap();
    calls
        .iter()
        .find(|(name, _)| name == "find_elements")
        .unwrap()
        .1
        .clone()
}

#[tokio::test]
async fn and_has_attribute_selects_rather_than_being_ignored() {
    // A plugin that ignores `attribute`, returning both buttons: the page check must still hold.
    let (mut c, calls) = client(Arc::new(|name, args| match name {
        "find_elements" => Ok(json!([
            el("k3f9e1", "button", "Delete everything"),
            el("k3f9e2", "button", "Save")
        ])
        .to_string()),
        "eval_js" => {
            assert!(args["code"].as_str().unwrap().contains("data-action"));
            Ok("[false,true]".to_string())
        }
        _ => Ok("{\"ok\":true}".to_string()),
    }))
    .await;
    let l = Locator::role("button").and_has_attribute("data-action", Some("save"));
    assert_eq!(l.count(&mut c).await.unwrap(), 1);
    l.click(&mut c).await.unwrap();
    let args = bridge_args(&calls);
    assert_eq!(args["attribute"], "data-action");
    assert_eq!(args["attribute_value"], "save");
    let calls = calls.lock().unwrap();
    assert_eq!(calls.last().unwrap().1["ref_id"], "k3f9e2");
}

/// `findElements` honouring its cap over a 120-row page.
fn page_of_rows(rows: usize) -> Handler {
    let page: Vec<Value> = (0..rows)
        .map(|i| el(&format!("k3f9e{i}"), "li", &format!("row {i}")))
        .collect();
    Arc::new(move |name, args| {
        if name != "find_elements" {
            return Ok("{\"ok\":true}".to_string());
        }
        let cap = args["max_results"].as_u64().unwrap_or(10) as usize;
        Ok(Value::Array(page.iter().take(cap).cloned().collect()).to_string())
    })
}

#[tokio::test]
async fn counts_and_positions_see_past_fifty_results() {
    let (mut c, calls) = client(page_of_rows(120)).await;
    assert_eq!(Locator::css("li").count(&mut c).await.unwrap(), 120);
    assert!(
        Locator::css("li")
            .expect(&mut c)
            .timeout_ms(30)
            .to_have_count(50)
            .await
            .is_err()
    );
    Locator::css("li").last().click(&mut c).await.unwrap();
    assert_eq!(
        calls.lock().unwrap().last().unwrap().1["ref_id"],
        "k3f9e119"
    );
    Locator::css("li").nth(60).click(&mut c).await.unwrap();
}

#[tokio::test]
async fn more_matches_than_the_cap_is_an_error_not_a_wrong_answer() {
    let (mut c, calls) = client(page_of_rows(1500)).await;
    let err = Locator::css("li").count(&mut c).await.unwrap_err();
    assert!(err.to_string().contains("1000"), "{err}");
    assert_eq!(bridge_args(&calls)["max_results"], 1001);
}

#[tokio::test]
async fn text_filters_are_applied_by_the_bridge_on_the_full_text() {
    let (mut c, calls) = client(answers_empty()).await;
    let _ = Locator::role("listitem")
        .and_text("row 75")
        .count(&mut c)
        .await;
    assert_eq!(bridge_args(&calls)["text"], "row 75");

    let (mut c, calls) = client(answers_empty()).await;
    let _ = Locator::role("listitem")
        .and_text_exact("row 75")
        .count(&mut c)
        .await;
    assert_eq!(bridge_args(&calls)["text_exact"], "row 75");

    let (mut c, calls) = client(answers_empty()).await;
    let _ = Locator::text_exact("Save").count(&mut c).await;
    let args = bridge_args(&calls);
    assert_eq!(args["text_exact"], "Save");
    assert!(args.get("text").is_none(), "{args}");
}

#[tokio::test]
async fn a_second_role_narrows_instead_of_replacing_the_first() {
    let (mut c, calls) = client(Arc::new(|_, _| {
        let mut button = el("k3f9e1", "button", "Go");
        button["role"] = json!("button");
        Ok(json!([button]).to_string())
    }))
    .await;
    let n = Locator::role("button")
        .and_role("link")
        .count(&mut c)
        .await
        .unwrap();
    assert_eq!(bridge_args(&calls)["role"], "button");
    assert_eq!(n, 0);
}

#[tokio::test]
async fn presence_and_count_include_hidden_elements() {
    let (mut c, calls) = client(answers_empty()).await;
    let _ = Locator::css("li").count(&mut c).await;
    assert_eq!(bridge_args(&calls)["include_hidden"], true);

    // A display:none element is still in the DOM.
    let hidden = || {
        let mut e = el("k3f9e1", "div", "");
        e["visible"] = json!(false);
        Ok(json!([e]).to_string())
    };
    let (mut c, _) = client(Arc::new(move |_, _| hidden())).await;
    let l = Locator::css("#h");
    assert!(
        l.expect(&mut c)
            .timeout_ms(30)
            .to_be_detached()
            .await
            .is_err()
    );
    l.expect(&mut c).to_be_attached().await.unwrap();
    assert!(!l.is_visible(&mut c).await.unwrap());
}

#[tokio::test]
async fn a_malformed_find_elements_answer_is_not_zero_elements() {
    // `window.__VICTAURI__?.findElements` on a page without the bridge evaluates to undefined.
    let (mut c, _) = client(Arc::new(|_, _| Ok("null".to_string()))).await;
    assert!(
        Locator::css("li")
            .expect(&mut c)
            .to_have_count(0)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn press_key_focuses_the_located_element_first() {
    let (mut c, calls) = client(Arc::new(|name, _| {
        if name == "find_elements" {
            Ok(json!([el("k3f9e7", "input", "")]).to_string())
        } else {
            Ok("{\"ok\":true}".to_string())
        }
    }))
    .await;
    Locator::test_id("search")
        .press_key(&mut c, "Enter")
        .await
        .unwrap();
    let calls = calls.lock().unwrap();
    let names: Vec<&str> = calls.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["find_elements", "interact", "input"]);
    assert_eq!(calls[1].1["action"], "focus");
    assert_eq!(calls[1].1["ref_id"], "k3f9e7");
    assert_eq!(calls[2].1["key"], "Enter");
}

// ── verify(): log windows ───────────────────────────────────────────────────

/// `logs`: the plugin keeps the newest `limit`, default 100.
fn log_with_old_delete(polls: usize) -> Handler {
    Arc::new(move |_, args| {
        let limit = args["limit"].as_u64().unwrap_or(100) as usize;
        let mut all = vec![json!({"command":"delete_account","args":{},"status":"ok",
            "url":"http://x/delete","method":"POST","level":"error","message":"old failure"})];
        all.extend(std::iter::repeat_n(
            json!({"command":"poll","args":{},"status":"ok","url":"http://x/poll",
                   "method":"GET","level":"log","message":"tick"}),
            polls,
        ));
        let n = all.len();
        Ok(Value::Array(all[n.saturating_sub(limit)..].to_vec()).to_string())
    })
}

#[tokio::test]
async fn verify_reads_the_whole_log_window() {
    let (mut c, _) = client(log_with_old_delete(150)).await;
    let r = c
        .verify()
        .ipc_was_not_called("delete_account")
        .no_network_request("/delete")
        .ipc_was_called("delete_account")
        .no_console_errors()
        .run()
        .await
        .unwrap();
    assert!(!r.results[0].passed, "{r:?}");
    assert!(!r.results[1].passed, "{r:?}");
    assert!(r.results[2].passed, "{r:?}");
    assert!(!r.results[3].passed, "{r:?}");
    assert_eq!(
        c.get_ipc_calls_for("delete_account").await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn a_negative_check_on_a_full_log_is_inconclusive() {
    let (mut c, _) = client(log_with_old_delete(5000)).await;
    let r = c
        .verify()
        .ipc_was_not_called("delete_account")
        .no_network_request("/delete")
        .run()
        .await
        .unwrap();
    for check in &r.results {
        assert!(!check.passed, "{check:?}");
        assert!(check.detail.contains("inconclusive"), "{check:?}");
    }
}

// ── verify(): page text ─────────────────────────────────────────────────────

#[tokio::test]
async fn has_text_reads_the_page_not_the_snapshot_json() {
    let page = "Saving failed: An unexpected error occurred while saving your document: disk full\n\
                He said \"hi\"\nbackend down\nFatal crash\nSave";
    let (mut c, calls) = client(Arc::new(move |name, _| {
        assert_eq!(name, "eval_js");
        Ok(serde_json::to_string(page).unwrap())
    }))
    .await;
    let r = c
        .verify()
        .has_text("disk full")
        .has_text("He said \"hi\"")
        .has_text("backend down")
        .has_text("Fatal crash")
        .has_no_text("stale_refs")
        .has_no_text("compact")
        .has_no_text("button")
        .run()
        .await
        .unwrap();
    assert!(r.all_passed(), "{r:?}");
    assert_eq!(calls.lock().unwrap().len(), 7);
}

#[tokio::test]
async fn has_no_text_fails_closed_without_a_page() {
    let (mut c, _) = client(Arc::new(|_, _| Ok("null".to_string()))).await;
    assert!(c.verify().has_no_text("Error").run().await.is_err());
}

// ── Waits and semantic assertions ───────────────────────────────────────────

#[tokio::test]
async fn a_builder_wait_that_timed_out_is_an_error() {
    let (mut c, _) = client(Arc::new(|_, _| {
        Ok(json!({"ok":false,"error":"timeout after 10ms","elapsed_ms":10}).to_string())
    }))
    .await;
    let err = c
        .wait("text")
        .value("Welcome")
        .timeout_ms(10)
        .run()
        .await
        .unwrap_err();
    assert!(matches!(err, TestError::Timeout(_)), "{err:?}");
}

#[tokio::test]
async fn a_failed_semantic_assertion_is_an_error() {
    let (mut c, _) = client(Arc::new(|_, _| {
        Ok(
            json!({"label":"x","passed":false,"actual":1,"expected":2,"message":"failed"})
                .to_string(),
        )
    }))
    .await;
    let err = c
        .assert_semantic("1", "x", "equals", json!(2))
        .await
        .unwrap_err();
    assert!(matches!(err, TestError::Assertion(_)), "{err:?}");
}

// ── Coverage ────────────────────────────────────────────────────────────────

fn registry_of_three() -> String {
    json!([
        {"name":"greet","description":null,"args":[],"return_type":"String","is_async":false},
        {"name":"get_counter","description":null,"args":[],"return_type":"i32","is_async":false},
        {"name":"delete_todo","description":null,"args":[],"return_type":"()","is_async":false}
    ])
    .to_string()
}

#[tokio::test]
async fn coverage_comes_from_the_plugins_own_view() {
    // `greet` is older than the log tool's newest-100 window, and `delete_todo` was mocked by a
    // route rule: `introspect coverage` got both right, reading the log directly.
    let (mut c, calls) = client(Arc::new(|name, args| {
        match (name, args["action"].as_str()) {
            ("get_registry", _) => Ok(registry_of_three()),
            ("introspect", Some("coverage")) => Ok(json!({
                "ipc_capture": {"transport":"fetch","capture":"full"},
                "registered_commands": 3, "invoked_commands": 2, "ipc_calls_observed": 151,
                "coverage_pct": 66.7, "uncovered": ["delete_todo"],
                "invoked_not_registered": [], "note": null
            })
            .to_string()),
            ("introspect", Some("command_timings")) => Ok(json!({
                "commands": [], "total_commands_profiled": 0, "saturated": false,
                "ipc_traffic": [
                    {"command":"get_counter","call_count":150,"avg_ms":1.0},
                    {"command":"greet","call_count":1,"avg_ms":1.0}
                ],
                "ipc_commands_observed": 2
            })
            .to_string()),
            _ => Ok("[]".to_string()),
        }
    }))
    .await;
    let report = victauri_test::coverage::coverage_report(&mut c)
        .await
        .unwrap();
    assert_eq!(report.tested_commands, 2);
    assert_eq!(report.untested, ["delete_todo"]);
    assert_eq!(report.most_called[0].name, "get_counter");
    assert_eq!(report.most_called[0].calls, 150);
    assert_eq!(report.note, None);
    assert!(
        !calls.lock().unwrap().iter().any(|(name, _)| name == "logs"),
        "the plugin's answer needs no log read"
    );
}

#[tokio::test]
async fn coverage_falls_back_for_a_plugin_without_it_and_says_so() {
    let (mut c, _) = client(Arc::new(|name, _| match name {
        "get_registry" => Ok(registry_of_three()),
        "introspect" => Err("unknown variant `coverage`".to_string()),
        "logs" => Ok(json!([
            {"command":"greet","status":"ok"},
            {"command":"delete_todo","status":"ok","mocked":true}
        ])
        .to_string()),
        _ => Ok("[]".to_string()),
    }))
    .await;
    let report = victauri_test::coverage::coverage_report(&mut c)
        .await
        .unwrap();
    assert_eq!(report.tested_commands, 1, "a mocked call is not coverage");
    assert!(
        report.note.as_deref().unwrap().contains("1000"),
        "{report:?}"
    );
    assert!(report.to_summary().contains("Note:"), "{report:?}");
}

#[tokio::test]
async fn coverage_says_when_the_page_ipc_cannot_be_seen() {
    let (mut c, _) = client(Arc::new(|name, args| {
        match (name, args["action"].as_str()) {
            ("get_registry", _) => Ok(registry_of_three()),
            ("introspect", Some("coverage")) => Ok(json!({
                "ipc_capture": {"transport":"post_message_fallback","capture":"none"},
                "registered_commands": 3, "invoked_commands": 0, "ipc_calls_observed": 0,
                "coverage_pct": 0.0, "uncovered": ["greet","get_counter","delete_todo"],
                "invoked_not_registered": [], "note": null
            })
            .to_string()),
            _ => Ok(json!({"ipc_traffic":[]}).to_string()),
        }
    }))
    .await;
    let report = victauri_test::coverage::coverage_report(&mut c)
        .await
        .unwrap();
    assert!(
        report.note.as_deref().unwrap().contains("none"),
        "{report:?}"
    );
}

// ── The deepest-match shape ─────────────────────────────────────────────────

#[tokio::test]
async fn a_text_locator_works_with_the_deepest_match_having_no_role() {
    // <button><span>Save</span></button>: bare text answers with the span, role null.
    let (mut c, calls) = client(Arc::new(|name, _| {
        if name == "find_elements" {
            Ok(json!([el("k3f9e4", "span", "Save")]).to_string())
        } else {
            Ok("{\"ok\":true}".to_string())
        }
    }))
    .await;
    Locator::text("Save").click(&mut c).await.unwrap();
    assert_eq!(calls.lock().unwrap().last().unwrap().1["ref_id"], "k3f9e4");

    // With a role, the bridge judges depth among elements having that role, so the query must
    // carry both for it to answer with the button; `and_role` must not filter the span client-side
    // out of a query that never named a role.
    let (mut c, calls) = client(Arc::new(|_, _| {
        let mut button = el("k3f9e3", "button", "Save");
        button["role"] = json!("button");
        Ok(json!([button]).to_string())
    }))
    .await;
    let n = Locator::text("Save")
        .and_role("button")
        .count(&mut c)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let args = bridge_args(&calls);
    assert_eq!(
        (&args["text"], &args["role"]),
        (&json!("Save"), &json!("button"))
    );
}

// ── Smoke helpers ───────────────────────────────────────────────────────────

#[tokio::test]
async fn an_empty_snapshot_is_not_valid() {
    let (mut c, _) = client(answers(r#"{"tree":"","stale_refs":[],"format":"compact"}"#)).await;
    assert!(c.assert_dom_snapshot_valid().await.is_err());
    let (mut c, _) = client(answers(
        r#"{"tree":"[k3f9e0] body\n","stale_refs":[],"format":"compact"}"#,
    ))
    .await;
    c.assert_dom_snapshot_valid().await.unwrap();
}

#[tokio::test]
async fn a_screenshot_without_image_data_is_not_ok() {
    let (mut c, _) = client(answers("no image here at all")).await;
    assert!(c.assert_screenshot_ok().await.is_err());
    let (mut c, _) = client(Arc::new(|_, _| {
        Err("screenshot capture failed: failed to handle capture frame".to_string())
    }))
    .await;
    assert!(c.assert_screenshot_ok().await.is_err());
    // A headless display is an environment limit, not a Victauri failure.
    let (mut c, _) = client(Arc::new(|_, _| {
        Err("the underlying handle is not available".to_string())
    }))
    .await;
    c.assert_screenshot_ok().await.unwrap();
}

#[tokio::test]
async fn blind_ipc_capture_is_not_healthy() {
    let (mut c, _) = client(answers(
        r#"{"healthy":true,"total_calls":0,"pending_count":0,"stale_count":0,"error_count":0,"stale_calls":[],"errored_calls":[],"ipc_capture":{"capture":"none"}}"#,
    ))
    .await;
    assert!(c.assert_ipc_integrity_ok().await.is_err());
    // Nothing observed yet is not blindness.
    let (mut c, _) = client(answers(
        r#"{"healthy":true,"total_calls":0,"pending_count":0,"stale_count":0,"error_count":0,"stale_calls":[],"errored_calls":[],"ipc_capture":{"capture":"unknown"}}"#,
    ))
    .await;
    c.assert_ipc_integrity_ok().await.unwrap();
}

#[tokio::test]
async fn text_by_id_is_the_text_content() {
    // `walkDom`'s `text` is direct text only: null for <p id=wrapper><span>hello</span></p>.
    let (mut c, _) = client(Arc::new(|name, _| {
        if name == "dom_snapshot" {
            Ok(
                json!({"tree":{"ref_id":"k3f9e0","tag":"body","text":null,"attributes":{},"children":[
                {"ref_id":"k3f9e1","tag":"p","text":null,"attributes":{"id":"wrapper"},"children":[
                    {"ref_id":"k3f9e2","tag":"span","text":"hello","attributes":{},"children":[]}]}
            ]},"stale_refs":[],"format":"json"})
                .to_string(),
            )
        } else {
            Ok("\"hello\"".to_string())
        }
    }))
    .await;
    assert_eq!(c.text_by_id("wrapper").await.unwrap(), "hello");
}

#[tokio::test]
async fn animation_scrub_with_capture_keeps_the_curve() {
    // The real order: `animation scrub` returns [image, text(meta with curve + manifest)].
    let app = axum::Router::new().route(
        "/mcp",
        axum::routing::post(|body: String| async move {
            let v: Value = serde_json::from_str(&body).unwrap_or_default();
            let id = v.get("id").cloned();
            axum::Json(match v["method"].as_str().unwrap_or("") {
                "initialize" => json!({"jsonrpc":"2.0","id":id,"result":{
                    "protocolVersion":"2025-06-18","capabilities":{},
                    "serverInfo":{"name":"m","version":"0"}}}),
                m if m.starts_with("notifications/") => json!({}),
                _ => json!({"jsonrpc":"2.0","id":id,"result":{"content":[
                    {"type":"image","data":"AAAA","mimeType":"image/png"},
                    {"type":"text","text":"{\"curve\":[{\"tx\":1}],\"filmstrip\":{\"manifest\":[]}}"}
                ],"isError":false}}),
            })
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut c = VictauriClient::connect_with_token(port, None)
        .await
        .unwrap();
    let r = c.animation_scrub(Some("#x"), 6, true).await.unwrap();
    assert_eq!(r["curve"][0]["tx"], 1, "{r}");
    assert_eq!(r["image"]["data"], "AAAA", "{r}");
}
