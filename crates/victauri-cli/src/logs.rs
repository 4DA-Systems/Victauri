//! `victauri logs` — read (or follow) a Tauri app's backend logs from the terminal.
//!
//! Live app: reads the structured backend log through the plugin's REST API
//! (`logs backend` / `backend_digest` / `stdout`). App down (crashed, rebuilding,
//! not started): falls back to the newest `victauri run` console capture on disk,
//! whose last record says how the app ended.
//!
//! `--follow` prints new lines as they arrive, so the command can be handed to an
//! agent harness's line-streaming monitor, e.g.
//! `victauri logs --follow --level warn` → one notification per backend warning.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// Most bytes of a console capture read at once (a capture rotates at 64 MB).
const MAX_READ: u64 = 32 * 1024 * 1024;

/// Options for `victauri logs`.
#[allow(clippy::struct_excessive_bools)]
pub struct LogsOptions {
    pub app: Option<String>,
    pub level: Option<String>,
    pub target: Option<String>,
    pub grep: Option<String>,
    pub limit: usize,
    pub follow: bool,
    pub digest: bool,
    pub stdout: bool,
    pub json: bool,
    pub capture: Option<PathBuf>,
    pub interval_ms: u64,
}

struct Backend {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl Backend {
    async fn connect(app: Option<&str>) -> std::result::Result<(Self, String), String> {
        let (port, token, label) = crate::bridge::resolve_backend(app).await?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        Ok((
            Self {
                http,
                base: format!("http://127.0.0.1:{port}"),
                token,
            },
            label,
        ))
    }

    async fn logs(&self, args: Value) -> Result<Value> {
        let mut req = self
            .http
            .post(format!("{}/api/tools/logs", self.base))
            .json(&args);
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let resp: Value = req
            .send()
            .await
            .context("app unreachable")?
            .json()
            .await
            .context("unexpected response")?;
        if let Some(err) = resp.get("error") {
            let err = err.as_str().map_or_else(|| err.to_string(), str::to_string);
            bail!(
                "{}",
                victauri_test::terminal::untrusted_multi_line(&explain_logs_error(&err))
            );
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }
}

/// A plugin older than 0.10.2 has no backend-log actions, and rejects them as an unknown
/// `logs` variant — the usual first contact after `cargo install victauri-cli` while the app
/// still embeds the older plugin. Say that, and how to fix it, instead of a deserializer error.
fn explain_logs_error(err: &str) -> String {
    let predates = ["backend", "backend_digest", "stdout"]
        .iter()
        .any(|action| err.contains(&format!("unknown variant `{action}`")));
    if predates {
        format!(
            "this app's Victauri plugin predates backend log capture (added in victauri-plugin \
             0.10.2) — bump the app's `victauri-plugin` dependency to 0.10.2 or newer and \
             rebuild it. (The plugin said: {err})"
        )
    } else {
        err.to_string()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn level_rank(level: &str) -> u8 {
    match level.to_ascii_lowercase().as_str() {
        "trace" => 0,
        "debug" => 1,
        "info" => 2,
        "warn" | "warning" => 3,
        "error" => 4,
        _ => 0,
    }
}

/// Drop ANSI CSI sequences (`ESC [ … final-byte`, e.g. the colour codes `tracing`'s fmt layer
/// writes) so captured output stays readable; any other control character is left for
/// [`clean`] to escape.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // Parameter and intermediate bytes, then one final byte in @..=~.
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Log text is app- (and often network-) controlled: one terminal line, with every control
/// and bidi character a visible escape, so it can neither break the line nor drive the
/// terminal.
fn clean(text: &str) -> String {
    victauri_test::terminal::single_line(&strip_ansi(text))
}

/// Print one line to stdout. A reader that went away (`| head`, a monitor that stopped
/// following) already has what it wanted: exit quietly instead of panicking like `println!`.
fn out(line: &str) {
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = writeln!(stdout, "{line}").and_then(|()| stdout.flush())
        && e.kind() == std::io::ErrorKind::BrokenPipe
    {
        std::process::exit(0);
    }
}

/// Render one structured backend entry as a single terminal line.
#[must_use]
pub fn render_entry(e: &Value) -> String {
    let ts = e["ts_ms"].as_u64().unwrap_or(0);
    let (secs, ms) = (ts / 1000, ts % 1000);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let level = e["level"].as_str().unwrap_or("?").to_ascii_uppercase();
    // One entry = one output line, always: a line-streaming consumer (a monitor that
    // turns each stdout line into an event) must never see a message split in two.
    let message = e["message"]
        .as_str()
        .unwrap_or("")
        .replace("\r\n", " \u{23ce} ")
        .replace('\n', " \u{23ce} ");
    let mut line = format!(
        "{h:02}:{m:02}:{s:02}.{ms:03}Z {level:<5} {}: {message}",
        e["target"].as_str().unwrap_or("")
    );
    if let Some(fields) = e["fields"].as_object() {
        for (k, v) in fields {
            if k == "backtrace" {
                continue;
            }
            match v {
                Value::String(s) => line.push_str(&format!(" {k}={s:?}")),
                other => line.push_str(&format!(" {k}={other}")),
            }
        }
    }
    if let Some(spans) = e["spans"].as_array().filter(|s| !s.is_empty()) {
        let path: Vec<&str> = spans.iter().filter_map(Value::as_str).collect();
        line.push_str(&format!("  [{}]", path.join(" > ")));
    }
    if e["source"] == "panic"
        && let Some(loc) = e["location"].as_str()
    {
        line.push_str(&format!("  at {loc}"));
    }
    clean(&line)
}

fn render_console(l: &Value) -> String {
    let ts = l["t"].as_u64().unwrap_or(0);
    let (secs, ms) = (ts / 1000, ts % 1000);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let stream = l["s"].as_str().unwrap_or("?");
    let tag = match stream {
        "err" => "ERR ",
        "exit" => "EXIT",
        _ => "OUT ",
    };
    let mut line = format!(
        "{h:02}:{m:02}:{s:02}.{ms:03}Z {tag} {}",
        l["l"].as_str().unwrap_or("")
    );
    if let Some(tail) = l["tail"].as_array().filter(|t| !t.is_empty()) {
        let last: Vec<&str> = tail.iter().filter_map(Value::as_str).collect();
        line.push_str(&format!(" | last stderr: {}", last.join(" \u{23ce} ")));
    }
    clean(&line)
}

fn emit(line: &str, json: bool, raw: &Value) {
    if json {
        out(&json_terminal_safe(&raw.to_string()));
    } else {
        out(line);
    }
}

/// `serde_json` escapes C0 controls but writes C1 controls (U+0080–U+009F, which some
/// terminals act on) and bidi controls raw. They can only occur inside JSON strings, so a
/// `\uXXXX` escape keeps the JSON valid and equal while making it inert in a terminal.
fn json_terminal_safe(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        let bidi = matches!(
            c,
            '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        );
        if bidi || ('\u{80}'..='\u{9f}').contains(&c) {
            out.push_str(&format!("\\u{:04x}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    out
}

/// Entry point for `victauri logs`.
///
/// # Errors
/// Fails when neither a live app nor a console capture is available, or on a
/// tool error reported by the app.
pub async fn cmd_logs(opts: LogsOptions) -> Result<()> {
    if opts.stdout || opts.capture.is_some() {
        return follow_capture(&opts, opts.capture.clone(), None, None).await;
    }
    let (backend, label) = match Backend::connect(opts.app.as_deref()).await {
        Ok(b) => b,
        // Apps ARE running but none can be used (several and no --app, or the one reached is
        // not the selected app): that is for the user to resolve, not a reason to show an old
        // capture instead.
        Err(why) if !why.starts_with("no running Victauri app") => {
            bail!("{}", victauri_test::terminal::untrusted_multi_line(&why))
        }
        Err(why) => {
            // App down: the console capture (if the app ran under `victauri run`) still
            // holds its output — including how it died. A capture does not record which app it
            // belongs to, so with `--app` the newest one could be another app's: say so instead.
            if opts.app.is_some() && crate::run::latest_capture().is_some() {
                bail!(
                    "{} — `victauri logs --stdout` shows the newest `victauri run` capture (of \
                     whichever app ran last), or pass `--capture <file>`",
                    clean(&why)
                );
            }
            if crate::run::latest_capture().is_some() {
                eprintln!(
                    "victauri logs: {} — showing the latest `victauri run` capture instead",
                    clean(&why)
                );
                return follow_capture(&opts, None, None, None).await;
            }
            bail!(
                "{}.\nStart the app (debug build), or launch it as `victauri run -- <dev command>` \
                 so its output is captured even while it is down.",
                clean(&why)
            );
        }
    };
    eprintln!("victauri logs: {}", clean(&label));

    if opts.digest {
        let d = backend
            .logs(json!({"action": "backend_digest", "limit": 10}))
            .await?;
        if opts.json {
            out(&json_terminal_safe(&serde_json::to_string_pretty(&d)?));
        } else {
            print_digest(&d);
        }
        return Ok(());
    }

    let mut args = json!({
        "action": "backend",
        "limit": opts.limit,
        "level": opts.level,
        "target": opts.target,
        "filter": opts.grep,
    });
    let first = backend.logs(args.clone()).await?;
    let sources = &first["sources"];
    // After a crash, the `victauri run` capture still holds the app's last words and exit status.
    let mut capture_path = own_capture_path(&first);
    if !follows_structured(sources) && sources["stdout_capture"] == true {
        eprintln!("victauri logs: no structured source in this app — reading its captured stdout");
        if opts.target.is_some() {
            eprintln!(
                "victauri logs: --target is ignored for raw stdout lines (they carry no target)"
            );
        }
        return follow_stdout_via_app(&backend, &opts, capture_path).await;
    }
    if let Some(hint) = first["hint"].as_str() {
        eprintln!("victauri logs: {}", clean(hint));
    }
    // `-n 0` = no history, only what arrives from now on.
    if opts.limit > 0 {
        print_page(&first, &opts);
    }
    if !opts.follow {
        return Ok(());
    }
    let mut backend = backend;
    let mut label = label;
    let mut cursor = first["next_seq"].as_u64().unwrap_or(0);
    // A cursor read returns the OLDEST entries after the cursor, `truncated` when more are
    // waiting: read the rest at once instead of one page per poll interval.
    let mut behind = false;
    // When the app last answered: what a dying process printed after this is its final moments.
    let mut last_ok_ms = now_ms();
    // Which process answered the last page (absent from plugins that do not report it).
    let mut pid = page_pid(&first);
    loop {
        if !behind {
            tokio::time::sleep(Duration::from_millis(opts.interval_ms)).await;
        }
        args["since_seq"] = cursor.into();
        args["limit"] = 2000.into();
        // A page from a NEW process behind the same port and token (a fixed token, or auth off)
        // is handled below as a restart, not printed as more of the old one — the new process's
        // entries below the old cursor would otherwise be skipped silently.
        if let Ok(page) = backend.logs(args.clone()).await
            && !is_new_process(&page, cursor, pid)
        {
            warn_gap(&page);
            print_page(&page, &opts);
            cursor = page["next_seq"].as_u64().unwrap_or(cursor);
            behind = page["truncated"] == true;
            last_ok_ms = now_ms();
            continue;
        }
        // Taken at the FIRST failed poll: the crash's last words are just before this moment
        // (taking it after the re-attach wait filtered every one of them out).
        let went_away_ms = now_ms();
        let stalled = std::time::Instant::now();
        // A slow answer is not a dead app, and a restarted app (a `tauri dev` rebuild) has a
        // new token and maybe a new port: re-resolve it through discovery for up to 20 s
        // rather than probing the unauthenticated /health, which a restarted app answers
        // while every authenticated read fails.
        let Some(Reattached {
            backend: b,
            label: l,
            page,
            restarted,
        }) = reattach(&opts, &args, cursor, &label, Duration::from_secs(20)).await
        else {
            eprintln!("victauri logs: app went away (crashed, closed or rebuilding)");
            return hand_off_to_capture(&opts, capture_path.clone(), went_away_ms).await;
        };
        if restarted {
            // The new process writes its own capture: a later crash must be read from THAT
            // file, not the previous process's — and the previous one's capture is the only
            // record of how it ended (its fatal panic rarely reaches a poll before it dies).
            let new_capture = own_capture_path(&page);
            if let Some(old) = capture_path.as_deref()
                && new_capture.as_deref() != Some(old)
            {
                show_final_moments(&opts, old, last_ok_ms);
            }
            eprintln!(
                "victauri logs: app restarted — following the new process {}",
                clean(&l)
            );
            capture_path = new_capture;
        } else {
            eprintln!(
                "victauri logs: app was unresponsive for {:.1}s — resumed",
                stalled.elapsed().as_secs_f64()
            );
        }
        warn_gap(&page);
        print_page(&page, &opts);
        cursor = page["next_seq"].as_u64().unwrap_or(0);
        behind = page["truncated"] == true;
        last_ok_ms = now_ms();
        pid = page_pid(&page);
        backend = b;
        label = l;
    }
}

fn page_pid(page: &Value) -> Option<u64> {
    page["sources"]["pid"].as_u64()
}

/// Whether a page came from a different process than the one followed so far: it reports
/// another pid, or (from a plugin that reports none) its cursor went backwards — sequence
/// numbers only go up within one process.
fn is_new_process(page: &Value, cursor: u64, known_pid: Option<u64>) -> bool {
    match (page_pid(page), known_pid) {
        (Some(now), Some(before)) if now != before => true,
        _ => went_backwards(page, cursor),
    }
}

/// `--follow` reads the structured log when the app built a structured source — even one that
/// has delivered nothing yet (an app that has not logged by the time `--follow` starts would
/// otherwise be switched to raw stdout for good).
fn follows_structured(sources: &Value) -> bool {
    sources["tracing"] == true || sources["log"] == true || sources["structured_installed"] == true
}

/// Sequence numbers only go up within one process, so a `next_seq` below the cursor means the
/// page came from a new process.
fn went_backwards(page: &Value, cursor: u64) -> bool {
    page["next_seq"].as_u64().is_some_and(|next| next < cursor)
}

fn warn_gap(page: &Value) {
    if page["gap"] == true {
        eprintln!("victauri logs: [some entries were evicted before they could be read]");
    }
}

/// The records of a previous process's capture worth showing when the follower moves on to a
/// restarted process: how it ended (its exit record, which carries its last stderr lines,
/// always) and any warning or error it printed from `since_ts_ms` on — its last successful
/// poll, so what the follower already printed is not repeated — that also passes the
/// follower's own `--level` / `--grep`. Not its routine output.
fn final_moments<'a>(lines: &'a [Value], since_ts_ms: u64, opts: &LogsOptions) -> Vec<&'a Value> {
    let picked: Vec<&Value> = lines
        .iter()
        .filter(|l| l["t"].as_u64().unwrap_or(0) >= since_ts_ms)
        .filter(|l| {
            l["s"] == "exit"
                || (l["level"]
                    .as_str()
                    .is_some_and(|lv| level_rank(lv) >= level_rank("warn"))
                    && console_line_passes(l, opts))
        })
        .collect();
    let keep = picked.len().saturating_sub(50);
    picked[keep..].to_vec()
}

fn show_final_moments(opts: &LogsOptions, path: &Path, since_ts_ms: u64) {
    let Ok((lines, _)) = read_capture(path, None) else {
        return;
    };
    let picked = final_moments(&lines, since_ts_ms, opts);
    if picked.is_empty() {
        return;
    }
    eprintln!("victauri logs: how the previous process ended (from its `victauri run` capture):");
    for l in picked {
        emit(&render_console(l), opts.json, l);
    }
}

/// Where this app's raw output is captured (when run under `victauri run`), from a `logs`
/// response's `sources`. The path comes from the server, so it is only used when it is one
/// of OUR captures — never an arbitrary file shown as the app's output.
fn own_capture_path(page: &Value) -> Option<PathBuf> {
    page["sources"]["stdout_capture_path"]
        .as_str()
        .map(PathBuf::from)
        .filter(|p| crate::run::is_own_capture(p))
}

struct Reattached {
    backend: Backend,
    label: String,
    page: Value,
    restarted: bool,
}

/// Re-resolve the app through discovery until `max` passes, and read the log from `cursor`.
/// A different process (the label carries the pid) or a sequence counter that went backwards
/// means the app restarted: then the NEW process's log is read from its start.
async fn reattach(
    opts: &LogsOptions,
    args: &Value,
    cursor: u64,
    old_label: &str,
    max: Duration,
) -> Option<Reattached> {
    let deadline = std::time::Instant::now() + max;
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let Ok((backend, label)) = Backend::connect(opts.app.as_deref()).await else {
            continue;
        };
        let Ok(page) = backend.logs(args.clone()).await else {
            continue;
        };
        let counter_reset = page["next_seq"].as_u64().is_some_and(|next| next < cursor);
        let restarted = label != old_label || counter_reset;
        let page = if restarted {
            let mut from_start = args.clone();
            from_start["since_seq"] = 0.into();
            match backend.logs(from_start).await {
                Ok(p) => p,
                Err(_) => continue,
            }
        } else {
            page
        };
        return Some(Reattached {
            backend,
            label,
            page,
            restarted,
        });
    }
    None
}

/// The app is gone: show what it printed in its final moments, then how it exited, from its
/// `victauri run` capture (if it ran under one).
async fn hand_off_to_capture(
    opts: &LogsOptions,
    capture_path: Option<PathBuf>,
    went_away_ms: u64,
) -> Result<()> {
    // Only the app's OWN capture. An app that reported none was not launched with `victauri
    // run`, so the newest capture on disk belongs to some other app (or an older run) — showing
    // it as this app's last words, or tailing a live one, would be wrong.
    let Some(path) = capture_path else {
        eprintln!(
            "victauri logs: no capture of this app's last output to show — it was not launched \
             with `victauri run` (`victauri run -- <dev command>` keeps one), or its capture was \
             written outside Victauri's private directory with `--capture` (read that file with \
             `victauri logs --capture <file>`)"
        );
        return Ok(());
    };
    follow_capture(
        opts,
        Some(path),
        Some(went_away_ms.saturating_sub(opts.interval_ms + 2000)),
        Some(Duration::from_secs(30)),
    )
    .await
}

fn print_page(page: &Value, opts: &LogsOptions) {
    if let Some(entries) = page["entries"].as_array() {
        for e in entries {
            emit(&render_entry(e), opts.json, e);
        }
    }
}

fn print_digest(d: &Value) {
    out(&clean(&format!(
        "buffered: {}   evicted: {}   counts: {}",
        d["buffered"], d["evicted"], d["buffered_counts"]
    )));
    if let Some(p) = d["panics"].as_array().filter(|p| !p.is_empty()) {
        out(&format!("\nPANICS ({}):", p.len()));
        for e in p {
            out(&format!("  {}", render_entry(e)));
        }
    }
    if let Some(r) = d["recent_problems"].as_array().filter(|r| !r.is_empty()) {
        out("\nRECENT WARNINGS/ERRORS:");
        for e in r {
            out(&format!("  {}", render_entry(e)));
        }
    }
    if let Some(r) = d["repeated"].as_array().filter(|r| !r.is_empty()) {
        out("\nREPEATED:");
        for t in r {
            out(&format!(
                "  {}",
                clean(&format!(
                    "{:>6}x {:<5} {}: {}",
                    t["count"],
                    t["level"].as_str().unwrap_or("").to_ascii_uppercase(),
                    t["target"].as_str().unwrap_or(""),
                    t["template"].as_str().unwrap_or("")
                ))
            ));
        }
    }
    if let Some(t) = d["top_targets"].as_array().filter(|t| !t.is_empty()) {
        out("\nNOISIEST TARGETS:");
        for x in t {
            out(&format!(
                "  {}",
                clean(&format!(
                    "{:>6}  {}",
                    x["count"],
                    x["target"].as_str().unwrap_or("")
                ))
            ));
        }
    }
    if let Some(h) = d["hint"].as_str() {
        out(&format!("\n{}", clean(h)));
    }
}

fn console_line_passes(l: &Value, opts: &LogsOptions) -> bool {
    if let Some(min) = &opts.level {
        // Lines whose level cannot be inferred are kept only when no level filter is set,
        // except the launcher's exit record, which always matters.
        let inferred = l["level"].as_str();
        if l["s"] != "exit" && !inferred.is_some_and(|lv| level_rank(lv) >= level_rank(min)) {
            return false;
        }
    }
    if let Some(g) = &opts.grep {
        return l["l"]
            .as_str()
            .is_some_and(|t| t.to_lowercase().contains(&g.to_lowercase()));
    }
    true
}

async fn follow_stdout_via_app(
    backend: &Backend,
    opts: &LogsOptions,
    capture_path: Option<PathBuf>,
) -> Result<()> {
    let mut args =
        json!({"action": "stdout", "limit": opts.limit, "level": opts.level, "filter": opts.grep});
    let mut first = true;
    loop {
        let page = match backend.logs(args.clone()).await {
            Ok(page) => page,
            // This mode is the one where the capture is everything: when the app dies while
            // being followed, read its last words and exit from the capture file directly.
            Err(e) if opts.follow && !first => {
                let went_away_ms = now_ms();
                eprintln!(
                    "victauri logs: app went away ({}) — reading its capture",
                    clean(&e.to_string())
                );
                return hand_off_to_capture(opts, capture_path, went_away_ms).await;
            }
            Err(e) => return Err(e),
        };
        if let Some(lines) = page["lines"].as_array() {
            for l in lines {
                emit(&render_console(l), opts.json, l);
            }
        }
        if !opts.follow {
            return Ok(());
        }
        args["since_seq"] = page["next_seq"].clone();
        if first {
            args["limit"] = 2000.into();
            first = false;
        }
        // A cursor read pages forward: while `truncated`, more is already waiting — read it
        // now, or a burst faster than one page per interval leaves the follower ever further
        // behind (and a capture that rotates meanwhile loses the unread rest).
        if page["truncated"] != true {
            tokio::time::sleep(Duration::from_millis(opts.interval_ms)).await;
        }
    }
}

/// Read (and with `--follow`, tail) a console capture file directly from disk.
/// `since_ts_ms` limits the initial print to records at/after that moment; with
/// `stop_after_idle` the follow ends at the app's exit record (or after that much silence).
async fn follow_capture(
    opts: &LogsOptions,
    path: Option<PathBuf>,
    since_ts_ms: Option<u64>,
    stop_after_idle: Option<Duration>,
) -> Result<()> {
    let path: PathBuf = match path {
        Some(p) => p,
        None => crate::run::latest_capture().context(
            "no `victauri run` console capture found — launch the app as `victauri run -- <dev command>`",
        )?,
    };
    eprintln!(
        "victauri logs: console capture {}",
        clean(&path.display().to_string())
    );
    let (lines, mut cursor) = read_capture(&path, None)?;
    let recent: Vec<&Value> = if let Some(ts) = since_ts_ms {
        lines
            .iter()
            .filter(|l| l["t"].as_u64().unwrap_or(0) >= ts)
            .collect()
    } else {
        let start = lines.len().saturating_sub(opts.limit);
        lines[start..].iter().collect()
    };
    let mut saw_exit = false;
    for l in recent {
        if console_line_passes(l, opts) {
            emit(&render_console(l), opts.json, l);
        }
        saw_exit |= l["s"] == "exit";
    }
    if !opts.follow || (saw_exit && stop_after_idle.is_some()) {
        return Ok(());
    }
    let mut idle = Duration::ZERO;
    loop {
        tokio::time::sleep(Duration::from_millis(opts.interval_ms)).await;
        let (lines, next) = read_capture(&path, Some(cursor))?;
        if lines.is_empty() {
            idle += Duration::from_millis(opts.interval_ms);
            if stop_after_idle.is_some_and(|max| idle >= max) {
                return Ok(());
            }
        } else {
            idle = Duration::ZERO;
        }
        for l in &lines {
            if console_line_passes(l, opts) {
                emit(&render_console(l), opts.json, l);
            }
            if l["s"] == "exit" && stop_after_idle.is_some() {
                return Ok(());
            }
        }
        cursor = next;
    }
}

/// Read JSONL capture records from `from` (or the whole file). Tolerates a
/// partially written last line and a file rotated under us.
fn read_capture(path: &Path, from: Option<u64>) -> Result<(Vec<Value>, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let len = f.metadata()?.len();
    // Without a cursor, read the newest `MAX_READ` bytes: a capture grows to 64 MB before it
    // rotates, and reading from the start showed lines from the middle of a long session —
    // never its last words or exit record.
    let tail_start = from.is_none() && len > MAX_READ;
    let mut start = from.unwrap_or_else(|| len.saturating_sub(MAX_READ));
    if start > len {
        start = 0;
    }
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.take(MAX_READ).read_to_end(&mut buf)?;
    let mut out = Vec::new();
    let mut consumed = 0usize;
    // A tail read starts mid-record: skip to the first complete line.
    if tail_start {
        match buf.iter().position(|b| *b == b'\n') {
            Some(i) => consumed = i + 1,
            None => return Ok((out, start + buf.len() as u64)),
        }
    }
    for chunk in buf[consumed..].split_inclusive(|b| *b == b'\n') {
        if !chunk.ends_with(b"\n") {
            break;
        }
        consumed += chunk.len();
        if let Ok(v) = serde_json::from_slice::<Value>(chunk) {
            let mut v = v;
            if v.get("level").is_none()
                && let Some(text) = v["l"].as_str()
                && let Some(level) = infer_level(text)
            {
                v["level"] = level.into();
            }
            out.push(v);
        }
    }
    Ok((out, start + consumed as u64))
}

/// Same heuristic as the plugin's (`backend_logs::infer_level`), kept local so the CLI
/// does not depend on the plugin crate.
fn infer_level(text: &str) -> Option<&'static str> {
    if text.contains("panicked at") || text.starts_with("error:") || text.starts_with("error[") {
        return Some("error");
    }
    if text.starts_with("warning:") {
        return Some("warn");
    }
    for token in text
        .split(|c: char| c.is_whitespace() || c == '[' || c == ']')
        .filter(|t| !t.is_empty())
        .take(4)
    {
        match token {
            "TRACE" => return Some("trace"),
            "DEBUG" => return Some("debug"),
            "INFO" => return Some("info"),
            "WARN" | "WARNING" => return Some("warn"),
            "ERROR" | "FATAL" => return Some("error"),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Release verification: the 0.10.2 CLI against 4DA's 0.10.1 plugin printed only the
    /// plugin's deserializer error for `victauri logs`.
    #[test]
    fn an_older_plugin_is_named_as_the_cause() {
        let old = "invalid parameters: unknown variant `backend`, expected one of `console`, \
                   `network`, `ipc`, `navigation`, `dialogs`, `events`, `slow_ipc`, `clear`";
        let msg = explain_logs_error(old);
        assert!(msg.contains("predates backend log capture"), "{msg}");
        assert!(msg.contains("0.10.2"), "{msg}");
        assert!(msg.contains(old), "the plugin's own words are kept: {msg}");
        for action in ["backend_digest", "stdout"] {
            let e = format!("invalid parameters: unknown variant `{action}`, expected one of …");
            assert!(explain_logs_error(&e).contains("predates"), "{action}");
        }
        let other = "no backend log source is active";
        assert_eq!(explain_logs_error(other), other);
    }

    /// Release review: an app that built `log_layer()` but had not logged yet when `--follow`
    /// started was switched to raw stdout for good (its `tracing`/`log` bits were still unset).
    #[test]
    fn a_built_but_silent_source_keeps_the_structured_follow() {
        let s = |tracing: bool, installed: bool| {
            json!({"tracing": tracing, "log": false, "structured_installed": installed,
                   "stdout_capture": true})
        };
        assert!(follows_structured(&s(true, true)));
        assert!(
            follows_structured(&s(false, true)),
            "built, nothing delivered yet"
        );
        assert!(!follows_structured(&s(false, false)));
        // A plugin from before the field existed: the old rule.
        assert!(!follows_structured(
            &json!({"tracing": false, "log": false})
        ));
    }

    /// Release review: with a fixed token (or auth off), a restart on the same port between two
    /// polls answered the old cursor with the new process's page, and the follower jumped its
    /// cursor back without noticing — skipping the new process's first entries.
    #[test]
    fn a_cursor_that_went_backwards_is_a_new_process() {
        assert!(went_backwards(&json!({"next_seq": 3}), 120));
        assert!(!went_backwards(&json!({"next_seq": 120}), 120), "caught up");
        assert!(!went_backwards(&json!({"next_seq": 121}), 120));
        assert!(
            !went_backwards(&json!({"error": "x"}), 120),
            "no next_seq: not a restart"
        );
    }

    /// Release review: a crash followed by a restart within the re-attach window showed only
    /// "app restarted" — the crash itself was never shown. The previous process's capture is
    /// read for how it ended: the exit record (with its last stderr lines) and any warning or
    /// error from its final moments, not its routine output.
    #[test]
    fn final_moments_are_how_the_process_ended() {
        let lines = vec![
            json!({"t": 100, "s": "err", "l": "old warning", "level": "warn"}),
            json!({"t": 1_000, "s": "out", "l": "routine", "level": "info"}),
            json!({"t": 1_001, "s": "out", "l": "no level at all"}),
            json!({"t": 1_002, "s": "err", "l": "thread 'main' panicked at src/x.rs", "level": "error"}),
            json!({"t": 1_003, "s": "exit", "l": "exited with code 101", "tail": ["boom"]}),
        ];
        let all = opts_with(None, None);
        let picked: Vec<&str> = final_moments(&lines, 900, &all)
            .into_iter()
            .map(|l| l["l"].as_str().unwrap())
            .collect();
        assert_eq!(
            picked,
            vec!["thread 'main' panicked at src/x.rs", "exited with code 101"],
            "before the window, and routine lines, are left out"
        );
        let many: Vec<Value> = (0..80)
            .map(|i| json!({"t": 2_000 + i, "s": "err", "l": format!("e{i}"), "level": "error"}))
            .collect();
        let tail = final_moments(&many, 0, &all);
        assert_eq!(tail.len(), 50);
        assert_eq!(tail[49]["l"], "e79", "the newest are kept");

        // The follower's own --grep / --level apply too; the exit record always shows.
        let warn_and_exit = vec![
            json!({"t": 5, "s": "err", "l": "disk nearly full", "level": "warn"}),
            json!({"t": 6, "s": "exit", "l": "exited with code 1"}),
        ];
        let errors_only = opts_with(Some("error"), None);
        let picked: Vec<&str> = final_moments(&warn_and_exit, 0, &errors_only)
            .into_iter()
            .map(|l| l["l"].as_str().unwrap())
            .collect();
        assert_eq!(
            picked,
            vec!["exited with code 1"],
            "--level error drops the warning"
        );
        let grep_other = opts_with(None, Some("database"));
        assert_eq!(final_moments(&warn_and_exit, 0, &grep_other).len(), 1);
    }

    fn opts_with(level: Option<&str>, grep: Option<&str>) -> LogsOptions {
        LogsOptions {
            app: None,
            level: level.map(str::to_string),
            target: None,
            grep: grep.map(str::to_string),
            limit: 100,
            follow: true,
            digest: false,
            stdout: false,
            json: false,
            capture: None,
            interval_ms: 500,
        }
    }

    /// Release review (second pass): a restart behind the same port and token was detected only
    /// when the new process's cursor fell below the old one — a new process that logged past it
    /// before the next poll was read as more of the old one. The plugin now reports its pid.
    #[test]
    fn a_page_from_another_pid_is_a_new_process() {
        let page = |pid: u64, next: u64| json!({"next_seq": next, "sources": {"pid": pid}});
        assert!(
            is_new_process(&page(2, 500), 100, Some(1)),
            "past the cursor, other pid"
        );
        assert!(
            !is_new_process(&page(1, 500), 100, Some(1)),
            "same process, more entries"
        );
        assert!(
            is_new_process(&page(1, 3), 100, Some(1)),
            "cursor went backwards"
        );
        // A plugin that reports no pid: the cursor rule alone.
        let old = |next: u64| json!({"next_seq": next, "sources": {}});
        assert!(!is_new_process(&old(500), 100, None));
        assert!(is_new_process(&old(3), 100, None));
    }

    #[test]
    fn a_capture_path_is_used_only_when_it_is_our_own_capture() {
        let Some(dir) = crate::run::capture_dir() else {
            return;
        };
        let ours = dir.join(format!("test-{}.jsonl", std::process::id()));
        std::fs::write(&ours, "").unwrap();
        let page = |p: &std::path::Path| json!({"sources": {"stdout_capture_path": p}});
        assert_eq!(own_capture_path(&page(&ours)), Some(ours.clone()));
        // A file outside the private capture directory (the server could name anything).
        let elsewhere =
            std::env::temp_dir().join(format!("victauri-not-ours-{}.jsonl", std::process::id()));
        std::fs::write(&elsewhere, "").unwrap();
        assert_eq!(own_capture_path(&page(&elsewhere)), None);
        assert_eq!(own_capture_path(&json!({"sources": {}})), None);
        let _ = std::fs::remove_file(&ours);
        let _ = std::fs::remove_file(&elsewhere);
    }

    #[test]
    fn render_entry_is_one_readable_line() {
        let e = json!({
            "ts_ms": 1_758_969_130_161_u64,
            "level": "warn",
            "target": "4da::scoring",
            "message": "Scoring context build exceeded the 30s soft ceiling",
            "fields": {"caller": "backfill_unscored", "elapsed_ms": 41191},
            "spans": ["analysis{run_type=\"background_deep\"}"],
            "source": "tracing"
        });
        let line = render_entry(&e);
        assert!(line.contains("WARN  4da::scoring: Scoring context build exceeded"));
        assert!(line.contains("caller=\"backfill_unscored\""));
        assert!(line.contains("elapsed_ms=41191"));
        assert!(line.contains("[analysis{run_type=\"background_deep\"}]"));
        assert!(!line.contains('\n'));
        let multi = json!({"ts_ms": 0, "level": "warn", "target": "t", "message": "a\nb\r\nc"});
        assert!(
            !render_entry(&multi).contains('\n'),
            "multi-line messages stay on one line"
        );
    }

    /// Log text is app- and often network-controlled (a fetched title, a peer's error): it
    /// must not drive the terminal, reorder text, or break the one-entry-one-line contract.
    #[test]
    fn hostile_log_text_cannot_drive_the_terminal() {
        let e = json!({
            "ts_ms": 0, "level": "error",
            "target": "app\u{1b}]0;pwned\u{7}",
            "message": "x\u{1b}[2J\u{202e}gnp.exe\r::error file=a::forged",
            "fields": {"k\u{1b}[31m": "v\u{1b}c"},
            "spans": ["s\u{2066}"]
        });
        let line = render_entry(&e);
        assert!(!line.chars().any(char::is_control), "{line:?}");
        assert!(
            !line.contains('\u{202e}') && !line.contains('\u{2066}'),
            "{line:?}"
        );
        let console = render_console(&json!({"t": 0, "s": "out",
            "l": "\u{1b}[32m INFO\u{1b}[0m app: ready\u{1b}]8;;http://x\u{7}"}));
        assert!(
            console.contains(" INFO app: ready"),
            "colours stripped: {console:?}"
        );
        assert!(!console.chars().any(char::is_control), "{console:?}");
    }

    #[test]
    fn strip_ansi_removes_csi_sequences_only() {
        assert_eq!(strip_ansi("\u{1b}[1;31mred\u{1b}[0m plain"), "red plain");
        // A non-CSI escape is left for `clean` to make visible, not silently dropped.
        assert_eq!(strip_ansi("a\u{1b}]0;t\u{7}b"), "a\u{1b}]0;t\u{7}b");
        assert_eq!(strip_ansi("trailing \u{1b}["), "trailing ");
    }

    #[test]
    fn capture_reader_infers_levels_and_skips_partial_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.jsonl");
        std::fs::write(
            &p,
            "{\"t\":1,\"s\":\"err\",\"l\":\"2026-09-27T10:00:00Z  WARN app: slow\"}\n{\"t\":2,\"s\":\"out\"",
        )
        .unwrap();
        let (lines, next) = read_capture(&p, None).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["level"], "warn");
        assert!(next > 0);
    }

    #[test]
    fn console_filter_keeps_exit_records_under_a_level_filter() {
        let opts = LogsOptions {
            app: None,
            level: Some("error".into()),
            target: None,
            grep: None,
            limit: 10,
            follow: false,
            digest: false,
            stdout: true,
            json: false,
            capture: None,
            interval_ms: 250,
        };
        assert!(console_line_passes(
            &json!({"s":"exit","l":"crashed: exit code 0xC0000409"}),
            &opts
        ));
        assert!(!console_line_passes(
            &json!({"s":"out","l":"hello","level":"info"}),
            &opts
        ));
        assert!(console_line_passes(
            &json!({"s":"err","l":"x","level":"error"}),
            &opts
        ));
    }
}
