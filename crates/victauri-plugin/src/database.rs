#[cfg(feature = "sqlite")]
use std::path::{Path, PathBuf};
#[cfg(feature = "sqlite")]
use std::sync::Arc;
#[cfg(feature = "sqlite")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "sqlite")]
use std::time::{Duration, Instant};

/// Arms a wall-clock watchdog that calls `Connection::interrupt()` at the deadline, then stops
/// and joins it on drop. The `progress_handler` only fires every N VDBE opcodes, so a single
/// long-running op (a big sort/scan, `quick_check` on a huge DB) can run well past the deadline
/// before the handler is consulted; the interrupt handle makes the CPU deadline a hard one.
#[cfg(feature = "sqlite")]
pub(crate) struct InterruptGuard {
    done: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(feature = "sqlite")]
impl InterruptGuard {
    pub(crate) fn arm(conn: &rusqlite::Connection, deadline: Duration) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let interrupt = conn.get_interrupt_handle();
        let done_for_thread = done.clone();
        // The watchdog PARKS until the deadline and is unparked on drop, so disarming is
        // immediate. (It used to sleep in fixed 25ms steps that the drop then joined — ~25ms
        // per guard however fast the guarded query was, which capped db_health at ~200 tables
        // per 5s budget even on a tiny database.)
        let handle = std::thread::spawn(move || {
            let start = Instant::now();
            loop {
                if done_for_thread.load(Ordering::Acquire) {
                    return;
                }
                let elapsed = start.elapsed();
                if elapsed >= deadline {
                    break;
                }
                std::thread::park_timeout(deadline - elapsed);
            }
            if !done_for_thread.load(Ordering::Acquire) {
                interrupt.interrupt();
            }
        });
        Self {
            done,
            handle: Some(handle),
        }
    }
}

#[cfg(feature = "sqlite")]
impl Drop for InterruptGuard {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            h.thread().unpark();
            let _ = h.join();
        }
    }
}

/// Outcome of one budgeted `SQLite` phase (see [`run_bounded`]).
#[cfg(feature = "sqlite")]
pub(crate) enum Bounded<T> {
    Done(T),
    TimedOut,
    Failed(String),
}

/// Run `f` under its OWN wall-clock budget: a progress handler plus a hard [`InterruptGuard`],
/// both scoped to this call and removed afterwards, so one slow phase cannot poison the phases
/// that follow it on the same connection.
#[cfg(feature = "sqlite")]
pub(crate) fn run_bounded<T>(
    conn: &rusqlite::Connection,
    budget: Duration,
    f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
) -> Bounded<T> {
    let started = Instant::now();
    let timed_out = Arc::new(AtomicBool::new(false));
    let marker = Arc::clone(&timed_out);
    conn.progress_handler(
        DB_HEALTH_PROGRESS_OPS,
        Some(move || {
            let expired = started.elapsed() >= budget;
            if expired {
                marker.store(true, Ordering::Relaxed);
            }
            expired
        }),
    );
    let result = {
        let _interrupt = InterruptGuard::arm(conn, budget);
        f(conn)
    };
    conn.progress_handler(DB_HEALTH_PROGRESS_OPS, None::<fn() -> bool>);
    match result {
        Ok(v) => Bounded::Done(v),
        Err(e)
            if timed_out.load(Ordering::Relaxed)
                || e.sqlite_error_code()
                    == Some(rusqlite::ffi::ErrorCode::OperationInterrupted) =>
        {
            Bounded::TimedOut
        }
        Err(e) => Bounded::Failed(e.to_string()),
    }
}

#[cfg(feature = "sqlite")]
const DB_HEALTH_PROGRESS_OPS: i32 = 10_000;
#[cfg(feature = "sqlite")]
const MAX_DB_HEALTH_TABLES: usize = 1_000;
#[cfg(feature = "sqlite")]
const MAX_DB_HEALTH_TABLE_BYTES: usize = 1_000_000;
#[cfg(feature = "sqlite")]
const MAX_DB_HEALTH_CELL_BYTES: i32 = 1_048_576;

/// Read-only health report for one `SQLite` database, in budgeted phases.
///
/// Every phase runs under its own budget, and a phase that runs out is REPORTED instead of
/// failing the whole call: the metadata PRAGMAs + table listing, the per-table `count(*)`s,
/// and `SQLite`'s `quick_check` (a full-file scan that dominates on large databases). Before
/// this, `quick_check` on a multi-GB database exhausted one shared deadline and the tool
/// returned only "timed out", discarding every cheap result with it.
///
/// The database file is treated as untrusted input: the connection is opened read-only with
/// `trusted_schema=OFF` and `SQLite`'s defensive mode, and virtual tables are listed but never
/// counted (counting one runs its module's code).
#[cfg(feature = "sqlite")]
pub(crate) fn db_health_report(
    path: &str,
    count_budget: Duration,
    check_budget: Duration,
) -> Result<serde_json::Value, String> {
    let conn = open_untrusted_read_only(path)?;
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
        MAX_DB_HEALTH_CELL_BYTES,
    );
    // The metadata phase runs `LIKE` against schema SQL the database file supplies.
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH,
        MAX_LIKE_PATTERN_BYTES,
    );

    // Phase 0: metadata + the (bounded) table listing.
    let meta = run_bounded(&conn, DB_HEALTH_META_BUDGET, |c| {
        let text = |name: &str| c.pragma_query_value(None, name, |r| r.get::<_, String>(0));
        let int = |name: &str| c.pragma_query_value(None, name, |r| r.get::<_, i64>(0));
        let journal_mode = text("journal_mode")?;
        let page_count = int("page_count")?;
        let page_size = int("page_size")?;
        let freelist_count = int("freelist_count")?;
        let mut stmt = c.prepare(
            "SELECT name, (sql LIKE 'CREATE VIRTUAL%') FROM sqlite_master \
             WHERE type='table' ORDER BY name",
        )?;
        let mut names: Vec<(String, bool)> = Vec::new();
        let mut table_bytes = 0usize;
        let mut truncated = false;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            let is_virtual: bool = row.get::<_, Option<bool>>(1)?.unwrap_or(false);
            if names.len() >= MAX_DB_HEALTH_TABLES
                || table_bytes.saturating_add(name.len()) > MAX_DB_HEALTH_TABLE_BYTES
            {
                truncated = true;
                break;
            }
            table_bytes = table_bytes.saturating_add(name.len());
            names.push((name, is_virtual));
        }
        Ok((
            journal_mode,
            page_count,
            page_size,
            freelist_count,
            names,
            truncated,
        ))
    });
    let (journal_mode, page_count, page_size, freelist_count, names, tables_truncated) = match meta
    {
        Bounded::Done(m) => m,
        Bounded::TimedOut => {
            return Err(format!(
                "database metadata did not load within {} ms",
                DB_HEALTH_META_BUDGET.as_millis()
            ));
        }
        Bounded::Failed(e) => return Err(format!("cannot read database metadata: {e}")),
    };
    let wal_checkpoint = if journal_mode == "wal" {
        "not run (read-only diagnostics)"
    } else {
        "n/a (not WAL mode)"
    };
    #[allow(clippy::cast_precision_loss)]
    let db_size_mb = page_count.saturating_mul(page_size) as f64 / (1024.0 * 1024.0);

    // Phase 1: per-table row counts under a shared count budget.
    let counts_started = Instant::now();
    let mut budget_exhausted = false;
    let mut all_counted = true;
    let mut tables = Vec::with_capacity(names.len());
    for (name, is_virtual) in names {
        let mut entry = serde_json::json!({ "name": name, "row_count": null });
        if is_virtual {
            entry["virtual"] = serde_json::json!(true);
            all_counted = false;
        } else {
            let remaining = count_budget.saturating_sub(counts_started.elapsed());
            if budget_exhausted || remaining.is_zero() {
                budget_exhausted = true;
                all_counted = false;
            } else {
                let sql = format!("SELECT count(*) FROM {}", quote_sqlite_identifier(&name));
                match run_bounded(&conn, remaining, |c| {
                    c.query_row(&sql, [], |r| r.get::<_, i64>(0))
                }) {
                    Bounded::Done(n) => entry["row_count"] = serde_json::json!(n),
                    Bounded::TimedOut => {
                        budget_exhausted = true;
                        all_counted = false;
                    }
                    Bounded::Failed(e) => {
                        entry["count_error"] = serde_json::json!(e);
                        all_counted = false;
                    }
                }
            }
        }
        tables.push(entry);
    }

    // Phase 2: integrity, on its own budget.
    let integrity = match run_bounded(&conn, check_budget, |c| {
        c.pragma_query_value(None, "quick_check", |r| r.get::<_, String>(0))
    }) {
        Bounded::Done(s) => s,
        Bounded::TimedOut => format!(
            "not completed: quick_check exceeded its {} ms budget on a {:.0} MB database \
             (a full-file scan; the result is unknown, not failed)",
            check_budget.as_millis(),
            db_size_mb
        ),
        Bounded::Failed(e) => format!("failed: {e}"),
    };

    Ok(serde_json::json!({
        "database": path,
        "journal_mode": journal_mode,
        "page_count": page_count,
        "page_size": page_size,
        "db_size_mb": (db_size_mb * 100.0).round() / 100.0,
        "freelist_count": freelist_count,
        "wal_checkpoint": wal_checkpoint,
        "integrity_check": integrity,
        "integrity_check_kind": "quick_check",
        "tables": tables,
        "tables_truncated": tables_truncated,
        // Every listed table has a row count (false if the budget ran out, a count failed, or a
        // table is virtual and deliberately not counted).
        "row_counts_complete": all_counted,
        "row_count_budget_exhausted": budget_exhausted,
    }))
}

/// Budget for the metadata PRAGMAs + table listing in [`db_health_report`].
#[cfg(feature = "sqlite")]
pub(crate) const DB_HEALTH_META_BUDGET: Duration = Duration::from_secs(3);

/// Open a database file Victauri did not create as read-only UNTRUSTED input: with
/// `trusted_schema=OFF` (schema-embedded SQL functions / virtual tables cannot run with side
/// effects) and `SQLite`'s defensive mode (no writes to shadow tables / schema corruption).
#[cfg(feature = "sqlite")]
pub(crate) fn open_untrusted_read_only(path: &str) -> Result<rusqlite::Connection, String> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("cannot open database: {e}"))?;
    conn.pragma_update(None, "trusted_schema", false)
        .map_err(|e| format!("cannot harden database connection: {e}"))?;
    conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
        .map_err(|e| format!("cannot harden database connection: {e}"))?;
    Ok(conn)
}
/// Quote an arbitrary table name as a `SQLite` identifier (`"…"`, embedded quotes doubled).
#[cfg(feature = "sqlite")]
pub(crate) fn quote_sqlite_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(feature = "sqlite")]
const MAX_ROWS_DEFAULT: usize = 100;
#[cfg(feature = "sqlite")]
const MAX_ROWS_LIMIT: usize = 10_000;
#[cfg(feature = "sqlite")]
const MAX_QUERY_CELL_BYTES: i32 = 1_048_576;
#[cfg(feature = "sqlite")]
const MAX_QUERY_RESULT_BYTES: usize = 5_000_000;
#[cfg(feature = "sqlite")]
const MAX_QUERY_SQL_BYTES: usize = 1_000_000;
#[cfg(feature = "sqlite")]
pub(crate) const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(feature = "sqlite")]
const QUERY_PROGRESS_OPS: i32 = 10_000;
/// Result-set width cap (`SQLite`'s default is 2000). With 1 MB cells, 2000 columns let a
/// single row reach gigabytes before any Rust-side budget could see it.
#[cfg(feature = "sqlite")]
const MAX_QUERY_COLUMNS: i32 = 256;
/// `LIKE`/`GLOB` pattern length cap (`SQLite`'s default is 50 000). Pattern matching is one
/// C call that never checks for an interrupt, so a long pattern against a long value ran for
/// tens of seconds past the query deadline.
#[cfg(feature = "sqlite")]
const MAX_LIKE_PATTERN_BYTES: i32 = 1_000;
/// How long a query waits on a lock held by the app. Kept short: a query holds its read
/// transaction for the lock wait PLUS its CPU deadline, and an open read transaction stalls
/// WAL checkpointing in the app.
#[cfg(feature = "sqlite")]
const QUERY_BUSY_TIMEOUT: Duration = Duration::from_secs(1);

#[cfg(feature = "sqlite")]
static READ_ONLY_PREFIXES: &[&str] = &["select", "pragma", "explain", "with"];

#[cfg(feature = "sqlite")]
fn strip_sql_comments(sql: &str) -> String {
    let mut result = String::with_capacity(sql.len());
    let bytes = sql.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if i + 1 < len && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
        } else if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < len {
                i += 2;
            }
            result.push(' ');
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
}

#[cfg(feature = "sqlite")]
fn is_read_only(sql: &str) -> bool {
    let cleaned = strip_sql_comments(sql);
    let trimmed = cleaned.trim_start().to_lowercase();
    if trimmed.is_empty() {
        return false;
    }
    READ_ONLY_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// Returns true if `sql` is the write form of a PRAGMA (`PRAGMA name = value`).
///
/// The read forms (`PRAGMA name`, `PRAGMA name(arg)`) are not flagged. An `=`
/// is only significant when it appears outside of any quoted string.
#[cfg(feature = "sqlite")]
fn is_pragma_write(sql: &str) -> bool {
    let cleaned = strip_sql_comments(sql);
    let trimmed = cleaned.trim_start();
    if !trimmed.to_lowercase().starts_with("pragma") {
        return false;
    }
    let bytes = trimmed.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    for &b in bytes {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'=' if !in_single && !in_double => return true,
            _ => {}
        }
    }
    false
}

/// Read-only / introspection PRAGMAs permitted on the user-facing `query` path.
///
/// A positive allowlist (audit C10): even without an `=`, some PRAGMAs have side
/// effects (`wal_checkpoint`, `optimize`, `incremental_vacuum`, `shrink_memory`,
/// `wal_checkpoint(TRUNCATE)`). The `READ_ONLY` open flag already blocks real
/// writes, but allowlisting the PRAGMA name makes the read-only contract explicit
/// and refuses side-effecting introspection outright.
#[cfg(feature = "sqlite")]
static SAFE_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "table_list",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
    "foreign_key_check",
    "collation_list",
    "database_list",
    "compile_options",
    "function_list",
    "module_list",
    "pragma_list",
    "journal_mode",
    "journal_size_limit",
    "page_count",
    "page_size",
    "max_page_count",
    "schema_version",
    "user_version",
    "application_id",
    "data_version",
    "freelist_count",
    "cache_size",
    "encoding",
    "auto_vacuum",
    "busy_timeout",
    "wal_autocheckpoint",
    "legacy_file_format",
    "locking_mode",
    "secure_delete",
    "synchronous",
    "temp_store",
    "mmap_size",
    "cache_spill",
    "cell_size_check",
    "integrity_check",
    "quick_check",
    "stats",
];

/// PRAGMAs on [`SAFE_PRAGMAS`] whose argument selects WHAT to read (a table, an index, a row
/// limit) rather than setting a value. Every other PRAGMA given an argument is a write
/// (`PRAGMA user_version(5)` is the same as `PRAGMA user_version = 5`).
#[cfg(feature = "sqlite")]
static READ_PRAGMAS_WITH_ARG: &[&str] = &[
    "table_info",
    "table_xinfo",
    "table_list",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
    "foreign_key_check",
    "integrity_check",
    "quick_check",
];

/// `SQLite` authorizer for agent-supplied `query_db` SQL: default-deny. Reads, SQL functions,
/// recursive CTEs, and allowlisted read-only PRAGMAs are permitted; everything else (writes,
/// ATTACH, schema changes, transactions, setter PRAGMAs) is refused before it runs.
#[cfg(feature = "sqlite")]
fn query_authorizer(ctx: rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization {
    use rusqlite::hooks::{AuthAction, Authorization};
    match ctx.action {
        AuthAction::Select
        | AuthAction::Read { .. }
        | AuthAction::Function { .. }
        | AuthAction::Recursive => Authorization::Allow,
        AuthAction::Pragma {
            pragma_name,
            pragma_value,
        } => {
            let name = pragma_name.to_ascii_lowercase();
            let allowed = SAFE_PRAGMAS.contains(&name.as_str())
                && (pragma_value.is_none() || READ_PRAGMAS_WITH_ARG.contains(&name.as_str()));
            if allowed {
                Authorization::Allow
            } else {
                Authorization::Deny
            }
        }
        _ => Authorization::Deny,
    }
}

/// Extract the lowercased PRAGMA name from a `PRAGMA [schema.]name ...` statement,
/// tolerating an optional `schema.` qualifier. Returns `None` for a non-PRAGMA or
/// a malformed one.
#[cfg(feature = "sqlite")]
fn pragma_name(sql: &str) -> Option<String> {
    let cleaned = strip_sql_comments(sql);
    let lower = cleaned.trim_start().to_lowercase();
    let stripped = lower.strip_prefix("pragma")?.trim_start();
    // Normalize away SQL identifier-quoting so quoted forms like `PRAGMA "main".table_info`
    // or `PRAGMA [main].table_info` parse to the same name as the bare form (avoids a
    // false-positive block of a legitimate quoted read PRAGMA).
    let normalized: String = stripped
        .chars()
        .filter(|c| !matches!(c, '"' | '`' | '[' | ']'))
        .collect();
    let rest = normalized.trim_start();
    // Optional `schema.` qualifier: only treat the part before the first '.' as a
    // schema when it's a bare identifier (no '(', '=', whitespace) — otherwise the
    // '.' belongs to a quoted arg and `rest` already starts with the name.
    let after_schema = match rest.split_once('.') {
        Some((maybe_schema, tail))
            if !maybe_schema.is_empty()
                && maybe_schema
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_') =>
        {
            tail
        }
        _ => rest,
    };
    let name: String = after_schema
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

/// True if `sql` is a PRAGMA whose name is NOT on [`SAFE_PRAGMAS`] (a malformed
/// PRAGMA is also rejected). Non-PRAGMA statements are not flagged here.
#[cfg(feature = "sqlite")]
fn is_disallowed_pragma(sql: &str) -> bool {
    let cleaned = strip_sql_comments(sql);
    if !cleaned.trim_start().to_lowercase().starts_with("pragma") {
        return false;
    }
    match pragma_name(sql) {
        Some(name) => !SAFE_PRAGMAS.contains(&name.as_str()),
        None => true,
    }
}

/// Discover `SQLite` database files in a directory (non-recursive, max depth 2).
#[cfg(feature = "sqlite")]
#[must_use]
pub fn discover_databases(dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let Ok(base) = std::fs::canonicalize(dir) else {
        return results;
    };
    discover_recursive(dir, &base, 0, 2, &mut results);
    results
}

#[cfg(feature = "sqlite")]
fn discover_recursive(
    dir: &Path,
    base: &Path,
    depth: u32,
    max_depth: u32,
    results: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() {
            continue;
        }
        // Windows junctions/reparse points are not always reported by
        // `Path::is_symlink`; canonical containment is the real boundary.
        let Ok(canonical) = std::fs::canonicalize(&path) else {
            continue;
        };
        if !canonical.starts_with(base) {
            continue;
        }
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str())
                && matches!(ext, "sqlite" | "sqlite3" | "db" | "sdb")
            {
                results.push(path);
            }
        } else if path.is_dir() && depth < max_depth {
            discover_recursive(&path, base, depth + 1, max_depth, results);
        }
    }
}

/// File basenames (matched case-insensitively, with or without a `SQLite` extension)
/// that are browser-engine internal databases, never the application's own DB
/// (Chromium/WebKit profile stores). Selecting one of these is the audit/red-team
/// "wrong database" bug: an agent would confidently inspect `WebView` state instead of
/// the app's data.
#[cfg(feature = "sqlite")]
const WEBVIEW_DB_BASENAMES: &[&str] = &[
    "cookies",
    "quotamanager",
    "web data",
    "history",
    "favicons",
    "top sites",
    "login data",
    "network action predictor",
    "transportsecurity",
    "trust tokens",
    "sharedstorage",
    "reporting and ntp",
    "media history",
    "affiliation database",
    "site characteristics database",
    "webdata",
];

/// Directory names (matched case-insensitively, anywhere in the path) that belong to a
/// `WebView`/browser engine's private storage area. Any `.db`/`.sqlite` under one of these
/// is an engine internal, not the app DB.
#[cfg(feature = "sqlite")]
const WEBVIEW_DIR_NAMES: &[&str] = &[
    "ebwebview",
    "wkwebview",
    "webkit",
    "local storage",
    "indexeddb",
    "session storage",
    "service worker",
    "gpucache",
    "code cache",
    "blob_storage",
    "shared proto db",
    "websql",
];

/// Whether a discovered database path is a `WebView`/browser-engine internal store rather
/// than the application's own database (audit / red-team "wrong DB" finding).
#[cfg(feature = "sqlite")]
#[must_use]
pub fn is_webview_internal(path: &Path) -> bool {
    if let Some(name) = path.file_stem().and_then(|n| n.to_str()) {
        let name = name.to_ascii_lowercase();
        if WEBVIEW_DB_BASENAMES.iter().any(|n| name == *n) {
            return true;
        }
    }
    path.components().any(|c| {
        let seg = c.as_os_str().to_string_lossy().to_ascii_lowercase();
        WEBVIEW_DIR_NAMES.iter().any(|d| seg == *d)
    })
}

/// A discovered database candidate with the metadata needed to disambiguate which DB the
/// application actually uses.
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DbCandidate {
    /// Absolute path to the discovered database file.
    pub path: PathBuf,
    /// File size in bytes (0 if it could not be stat'd).
    pub size_bytes: u64,
    /// Whether this is a `WebView`/browser-engine internal store rather than an app DB.
    pub webview_internal: bool,
}

/// Classify every database discovered under `dirs`, returning application candidates first
/// (non-`WebView`, largest by size — the substantial app DB outranks incidental ones) and
/// `WebView` internals last. De-duplicates paths discovered via overlapping roots.
#[cfg(feature = "sqlite")]
#[must_use]
pub fn classify_databases(dirs: &[PathBuf]) -> Vec<DbCandidate> {
    let mut seen = std::collections::HashSet::new();
    let mut candidates: Vec<DbCandidate> = Vec::new();
    for dir in dirs {
        for path in discover_databases(dir) {
            let key = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if !seen.insert(key) {
                continue;
            }
            let size_bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
            let webview_internal = is_webview_internal(&path);
            candidates.push(DbCandidate {
                path,
                size_bytes,
                webview_internal,
            });
        }
    }
    // Application DBs first, then by size descending (larger ⇒ more likely the real DB).
    candidates.sort_by(|a, b| {
        a.webview_internal
            .cmp(&b.webview_internal)
            .then(b.size_bytes.cmp(&a.size_bytes))
    });
    candidates
}

/// Select the single most likely application database from `dirs`, excluding `WebView`
/// internals.
///
/// # Errors
/// Returns `Err` with a diagnostic when no application database is found — either no
/// databases at all, or only `WebView`/browser-engine internal stores (the error lists
/// the skipped internals so the caller can tell an agent to register the real DB
/// directory via `db_search_paths` or pass an explicit `path`).
#[cfg(feature = "sqlite")]
pub fn select_app_database(dirs: &[PathBuf]) -> Result<PathBuf, String> {
    let candidates = classify_databases(dirs);
    if let Some(app) = candidates.iter().find(|c| !c.webview_internal) {
        return Ok(app.path.clone());
    }
    if candidates.is_empty() {
        let dirs_str = dirs
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!("no SQLite databases found in: {dirs_str}"));
    }
    let internals = candidates
        .iter()
        .map(|c| c.path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "only WebView/browser-engine internal databases were found ({internals}); none looks \
         like an application database. Register the app's DB directory via \
         VictauriBuilder::db_search_paths, or pass an explicit `path`."
    ))
}

/// Execute a read-only SQL query against a `SQLite` database.
///
/// # Errors
///
/// Returns an error if the query is not read-only, the database cannot be opened,
/// or the query fails.
#[cfg(feature = "sqlite")]
pub fn query(
    db_path: &Path,
    sql: &str,
    params: &[serde_json::Value],
    max_rows: Option<usize>,
) -> Result<serde_json::Value, String> {
    query_with_limits(
        db_path,
        sql,
        params,
        max_rows,
        QUERY_TIMEOUT,
        MAX_QUERY_RESULT_BYTES,
    )
}

/// The checks a query must pass before any database is even opened: length, read-only
/// statement kind, no PRAGMA write or side-effecting PRAGMA, no stacked statements. The
/// `query_db` tool runs them BEFORE resolving which database to open, so a refused query is
/// refused for what it is — whether or not the app has a database — and touches no file.
///
/// # Errors
///
/// Returns the refusal message for the first check the query fails.
#[cfg(feature = "sqlite")]
pub fn validate_query(sql: &str) -> Result<(), String> {
    if sql.len() > MAX_QUERY_SQL_BYTES {
        return Err(format!(
            "query exceeds maximum length ({MAX_QUERY_SQL_BYTES} bytes)"
        ));
    }
    if !is_read_only(sql) {
        return Err(
            "only SELECT, PRAGMA, EXPLAIN, and WITH queries are allowed (read-only access)"
                .to_string(),
        );
    }

    // Defence in depth: the connection is opened READ_ONLY (SQLite rejects
    // actual writes), but explicitly reject the write form of PRAGMA
    // (`PRAGMA name = value`) so the read-only contract is self-evident and
    // not solely reliant on the open flags. The read forms `PRAGMA name` and
    // `PRAGMA name(arg)` remain allowed.
    if is_pragma_write(sql) {
        return Err(
            "PRAGMA writes (PRAGMA name = value) are not allowed (read-only access)".to_string(),
        );
    }

    // Positive PRAGMA allowlist (audit C10): reject side-effecting PRAGMAs
    // (wal_checkpoint, optimize, incremental_vacuum, …) even without an `=`.
    if is_disallowed_pragma(sql) {
        return Err(
            "only read-only introspection PRAGMAs are allowed (e.g. table_info, \
             integrity_check, page_count); side-effecting PRAGMAs such as \
             wal_checkpoint/optimize/incremental_vacuum are blocked"
                .to_string(),
        );
    }

    let cleaned = strip_sql_comments(sql);
    if cleaned.contains(';') {
        let parts: Vec<&str> = cleaned
            .split(';')
            .filter(|s| !s.trim().is_empty())
            .collect();
        if parts.len() > 1 {
            return Err(
                "stacked queries (multiple statements separated by ;) are not allowed".to_string(),
            );
        }
    }
    Ok(())
}

#[cfg(feature = "sqlite")]
fn query_with_limits(
    db_path: &Path,
    sql: &str,
    params: &[serde_json::Value],
    max_rows: Option<usize>,
    query_timeout: Duration,
    max_result_bytes: usize,
) -> Result<serde_json::Value, String> {
    validate_query(sql)?;

    let max_rows = max_rows.unwrap_or(MAX_ROWS_DEFAULT).min(MAX_ROWS_LIMIT);

    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("failed to open database: {e}"))?;
    // The database is untrusted input (see `open_untrusted_read_only`), and the query is
    // agent-supplied: SQLite's own AUTHORIZER is the enforcement point, not string parsing.
    // It sees every PRAGMA SQLite is about to run — statement form, `PRAGMA name(arg)`, and
    // table-valued `pragma_*()` functions alike (the string checks above missed the last two).
    conn.pragma_update(None, "trusted_schema", false)
        .map_err(|e| format!("failed to harden database connection: {e}"))?;
    conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
        .map_err(|e| format!("failed to harden database connection: {e}"))?;
    conn.authorizer(Some(query_authorizer));

    // Limit lock waits separately from the CPU deadline enforced below.
    conn.busy_timeout(QUERY_BUSY_TIMEOUT)
        .map_err(|e| format!("failed to set timeout: {e}"))?;

    // Bound both SQLite's per-value/row allocation and CPU time. `busy_timeout`
    // only limits lock waits; it does not stop a CPU-heavy recursive query.
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
        MAX_QUERY_CELL_BYTES,
    );
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_SQL_LENGTH,
        MAX_QUERY_SQL_BYTES as i32,
    );
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH,
        MAX_LIKE_PATTERN_BYTES,
    );
    let started = Instant::now();
    conn.progress_handler(
        QUERY_PROGRESS_OPS,
        Some(move || started.elapsed() >= query_timeout),
    );
    // Hard wall-clock backstop: the progress handler under-samples a single long op, so also
    // arm an interrupt-handle watchdog. Lives until the query completes (drops/joins on every
    // return path, including `?` errors below).
    let _interrupt = InterruptGuard::arm(&conn, query_timeout);

    // `SQLITE_LIMIT_COLUMN` also bounds table DEFINITIONS, so a schema holding one wide table
    // would fail to parse under it and make the whole database unqueryable. Load the schema
    // first (preparing any statement does), then cap the width of the query's result set.
    drop(
        conn.prepare("SELECT 1 FROM sqlite_master LIMIT 0")
            .map_err(|e| sqlite_query_error("failed to load database schema", e, query_timeout))?,
    );
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_COLUMN,
        MAX_QUERY_COLUMNS,
    );

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| sqlite_query_error("failed to prepare query", e, query_timeout))?;

    let column_names = unique_column_keys(&stmt.column_names());
    // `"key":` per column, sized once.
    let key_bytes: Vec<usize> = column_names.iter().map(|k| json_len(k) + 1).collect();
    // A row object's fixed cost: its braces and the commas between its fields.
    let row_overhead = 2 + column_names.len().saturating_sub(1);

    let sqlite_params: Vec<Box<dyn rusqlite::types::ToSql>> =
        params.iter().map(json_to_sql).collect();
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = sqlite_params.iter().map(|b| &**b).collect();

    let mut rows_out: Vec<serde_json::Value> = Vec::new();
    let mut rows = stmt
        .query(param_refs.as_slice())
        .map_err(|e| sqlite_query_error("query execution failed", e, query_timeout))?;
    let mut result_bytes = json_len(&column_names);
    let mut truncated = false;

    // The byte budget is charged CELL BY CELL, from each raw value's encoded size, before the
    // cell is converted: a row can never be materialized past the cap (it used to be built,
    // base64'd and re-serialized in full before the first size check).
    'rows: while let Some(row) = rows
        .next()
        .map_err(|e| sqlite_query_error("row read failed", e, query_timeout))?
    {
        if rows_out.len() >= max_rows {
            truncated = true;
            break;
        }
        let mut row_bytes = row_overhead;
        let mut obj = serde_json::Map::new();
        for (i, col_name) in column_names.iter().enumerate() {
            let spent = result_bytes
                .saturating_add(row_bytes)
                .saturating_add(key_bytes[i]);
            let budget = max_result_bytes.saturating_sub(spent);
            let Some((value, value_bytes)) = cell_to_json(row, i, budget) else {
                truncated = true;
                break 'rows;
            };
            row_bytes = row_bytes
                .saturating_add(key_bytes[i])
                .saturating_add(value_bytes);
            obj.insert(col_name.clone(), value);
        }
        if result_bytes.saturating_add(row_bytes) > max_result_bytes {
            truncated = true;
            break;
        }
        result_bytes = result_bytes.saturating_add(row_bytes);
        rows_out.push(serde_json::Value::Object(obj));
    }

    Ok(serde_json::json!({
        "columns": column_names,
        "rows": rows_out,
        "row_count": rows_out.len(),
        "truncated": truncated,
        "max_rows": max_rows,
        "result_bytes": result_bytes,
        "max_result_bytes": max_result_bytes,
    }))
}

#[cfg(feature = "sqlite")]
fn sqlite_query_error(context: &str, error: rusqlite::Error, timeout: Duration) -> String {
    if error.sqlite_error_code() == Some(rusqlite::ffi::ErrorCode::OperationInterrupted) {
        format!(
            "{context}: query timed out after {} ms",
            timeout.as_millis()
        )
    } else {
        format!("{context}: {error}")
    }
}

#[cfg(feature = "sqlite")]
fn json_to_sql(val: &serde_json::Value) -> Box<dyn rusqlite::types::ToSql> {
    match val {
        serde_json::Value::Null => Box::new(rusqlite::types::Null),
        serde_json::Value::Bool(b) => Box::new(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Box::new(i)
            } else if let Some(f) = n.as_f64() {
                Box::new(f)
            } else {
                Box::new(n.to_string())
            }
        }
        serde_json::Value::String(s) => Box::new(s.clone()),
        other => Box::new(other.to_string()),
    }
}

/// Result-object keys for a statement's columns. `SQLite` allows duplicate result names
/// (`SELECT 1 AS a, 2 AS a`), which as JSON object keys silently dropped all but one value
/// while `columns` still listed both. A repeated name gets a `:N` suffix (`a`, `a:1`, …),
/// skipping any suffix that is itself a real column name, so every key is unique and
/// `columns` lists exactly the keys each row carries.
#[cfg(feature = "sqlite")]
fn unique_column_keys(names: &[&str]) -> Vec<String> {
    let originals: std::collections::HashSet<&str> = names.iter().copied().collect();
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    names
        .iter()
        .map(|&name| {
            let mut key = name.to_string();
            let mut n = 0u32;
            while used.contains(&key) || (n > 0 && originals.contains(key.as_str())) {
                n += 1;
                key = format!("{name}:{n}");
            }
            used.insert(key.clone());
            key
        })
        .collect()
}

/// A `std::io::Write` sink that only counts bytes, so an encoded size can be measured
/// without building the encoding.
#[cfg(feature = "sqlite")]
struct ByteCounter(usize);

#[cfg(feature = "sqlite")]
impl std::io::Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Exact compact-JSON length of `value`, measured without allocating the encoding.
#[cfg(feature = "sqlite")]
fn json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    let mut counter = ByteCounter(0);
    // Serializing into a counter cannot fail for these value types; a failure would only
    // under-count, and the caller's cap check is then conservative on the next cell.
    let _ = serde_json::to_writer(&mut counter, value);
    counter.0
}

/// Convert one result cell to JSON if its encoded size fits `budget` bytes, returning the value
/// and that size. The size is taken from the RAW cell before converting (a blob's base64 length
/// is computed, not encoded), so a cell that does not fit is never materialized.
#[cfg(feature = "sqlite")]
fn cell_to_json(
    row: &rusqlite::Row,
    idx: usize,
    budget: usize,
) -> Option<(serde_json::Value, usize)> {
    use rusqlite::types::ValueRef;
    let (value, bytes) = match row.get_ref(idx) {
        Ok(ValueRef::Null) | Err(_) => (serde_json::Value::Null, 4),
        Ok(ValueRef::Integer(i)) => (serde_json::json!(i), json_len(&i)),
        Ok(ValueRef::Real(f)) => {
            let v = serde_json::json!(f);
            let n = json_len(&v);
            (v, n)
        }
        Ok(ValueRef::Text(t)) => {
            // Borrowed (no copy) for valid UTF-8; a cell is at most MAX_QUERY_CELL_BYTES.
            let s = String::from_utf8_lossy(t);
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&s)
                && (parsed.is_object() || parsed.is_array())
            {
                let n = json_len(&parsed);
                (parsed, n)
            } else {
                let n = json_len(&*s);
                if n > budget {
                    return None;
                }
                (serde_json::Value::String(s.into_owned()), n)
            }
        }
        Ok(ValueRef::Blob(b)) => {
            use base64::Engine;
            // `{"__blob":true,"size":N,"base64":"…"}`
            let bytes = r#"{"__blob":true,"size":,"base64":""}"#.len()
                + json_len(&b.len())
                + b.len().div_ceil(3) * 4;
            if bytes > budget {
                return None;
            }
            let v = serde_json::json!({
                "__blob": true,
                "size": b.len(),
                "base64": base64::engine::general_purpose::STANDARD.encode(b),
            });
            (v, bytes)
        }
    };
    (bytes <= budget).then_some((value, bytes))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;

    fn create_test_db() -> (tempfile::NamedTempFile, PathBuf) {
        let file = tempfile::NamedTempFile::with_suffix(".sqlite").unwrap();
        let path = file.path().to_path_buf();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, score REAL);
             INSERT INTO users VALUES (1, 'Alice', 95.5);
             INSERT INTO users VALUES (2, 'Bob', 87.0);
             INSERT INTO users VALUES (3, 'Charlie', 92.3);",
        )
        .unwrap();
        (file, path)
    }

    #[test]
    fn db_health_report_complete_within_budget() {
        let (_f, path) = create_test_db();
        let long = Duration::from_secs(30);
        let r = db_health_report(path.to_str().unwrap(), long, long).unwrap();
        assert_eq!(r["integrity_check"], "ok");
        assert_eq!(r["row_counts_complete"], true);
        assert_eq!(r["tables"][0]["name"], "users");
        assert_eq!(r["tables"][0]["row_count"], 3);
        assert!(r["page_count"].as_i64().unwrap() > 0);
    }

    /// A database too big for the budgets must still return every cheap result, with the
    /// slow phases REPORTED as incomplete — not fail the whole call (the live 4DA 1.4 GB case).
    #[test]
    fn db_health_report_degrades_instead_of_failing_when_budgets_run_out() {
        let file = tempfile::NamedTempFile::with_suffix(".sqlite").unwrap();
        let path = file.path().to_path_buf();
        let conn = rusqlite::Connection::open(&path).unwrap();
        // Enough rows that quick_check needs far more than DB_HEALTH_PROGRESS_OPS VDBE ops,
        // so a zero budget deterministically trips the progress handler.
        conn.execute_batch(
            "CREATE TABLE big (id INTEGER PRIMARY KEY, v TEXT);
             CREATE INDEX big_v ON big(v);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 50000)
             INSERT INTO big SELECT i, hex(randomblob(16)) FROM n;",
        )
        .unwrap();
        drop(conn);

        let r = db_health_report(path.to_str().unwrap(), Duration::ZERO, Duration::ZERO)
            .expect("a timed-out phase must not fail the whole report");
        assert_eq!(r["row_counts_complete"], false);
        assert!(r["tables"][0]["row_count"].is_null());
        assert_eq!(r["tables"][0]["name"], "big");
        let integrity = r["integrity_check"].as_str().unwrap();
        assert!(integrity.starts_with("not completed"), "got: {integrity}");
        // The cheap metadata survives.
        assert!(r["page_count"].as_i64().unwrap() > 0);
        assert!(r["journal_mode"].is_string());
    }

    /// Manual check against a REAL database (read-only):
    /// `VICTAURI_DB_HEALTH_PATH=/path/app.db cargo test -p victauri-plugin --lib db_health_real -- --ignored --nocapture`
    #[test]
    #[ignore = "needs VICTAURI_DB_HEALTH_PATH pointing at a real database"]
    fn db_health_real_database() {
        let path = std::env::var("VICTAURI_DB_HEALTH_PATH").expect("set VICTAURI_DB_HEALTH_PATH");
        let started = Instant::now();
        let r = db_health_report(&path, Duration::from_secs(5), Duration::from_secs(5)).unwrap();
        println!("elapsed: {:?}", started.elapsed());
        println!("{}", serde_json::to_string_pretty(&r).unwrap());
    }

    #[test]
    fn authorizer_blocks_pragma_function_and_parenthesized_writes() {
        let (_f, path) = create_test_db();
        // Table-valued pragma functions went through the SELECT prefix check unchecked.
        let err = query(&path, "SELECT * FROM pragma_optimize", &[], None).unwrap_err();
        assert!(
            err.contains("not authorized") || err.contains("prohibited"),
            "{err}"
        );
        // `PRAGMA name(arg)` is the write form for setter pragmas, with no `=` to catch.
        for sql in ["PRAGMA user_version(5)", "PRAGMA journal_mode(DELETE)"] {
            assert!(query(&path, sql, &[], None).is_err(), "must block: {sql}");
        }
        // Legitimate read forms still work — statement, argument, and function forms.
        assert!(query(&path, "PRAGMA table_info(users)", &[], None).is_ok());
        assert!(query(&path, "PRAGMA user_version", &[], None).is_ok());
        let r = query(
            &path,
            "SELECT name FROM pragma_table_info('users')",
            &[],
            None,
        )
        .unwrap();
        assert_eq!(r["row_count"], 3);
        assert!(query(&path, "SELECT count(*) FROM users", &[], None).is_ok());
    }

    #[test]
    fn db_health_counts_hundreds_of_tables_within_budget() {
        // Each count used to cost a fixed ~25ms (the interrupt watchdog's sleep step, joined
        // on drop), capping a 5s budget at ~200 tables even on a tiny database.
        let file = tempfile::NamedTempFile::with_suffix(".sqlite").unwrap();
        let conn = rusqlite::Connection::open(file.path()).unwrap();
        let mut ddl = String::new();
        for i in 0..400 {
            ddl.push_str(&format!(
                "CREATE TABLE t{i} (x INTEGER); INSERT INTO t{i} VALUES (1);"
            ));
        }
        conn.execute_batch(&ddl).unwrap();
        drop(conn);
        let started = Instant::now();
        let r = db_health_report(
            file.path().to_str().unwrap(),
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(r["tables"].as_array().unwrap().len(), 400);
        assert_eq!(r["row_counts_complete"], true, "{r}");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "400 trivial counts took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn db_health_lists_but_never_counts_virtual_tables() {
        let file = tempfile::NamedTempFile::with_suffix(".sqlite").unwrap();
        let conn = rusqlite::Connection::open(file.path()).unwrap();
        conn.execute_batch("CREATE TABLE plain (x); INSERT INTO plain VALUES (1);")
            .unwrap();
        if conn
            .execute_batch("CREATE VIRTUAL TABLE docs USING fts5(body);")
            .is_err()
        {
            return; // this SQLite build has no FTS5; nothing to test
        }
        drop(conn);
        let long = Duration::from_secs(10);
        let r = db_health_report(file.path().to_str().unwrap(), long, long).unwrap();
        let docs = r["tables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "docs")
            .expect("virtual table is listed");
        assert_eq!(docs["virtual"], true);
        assert!(
            docs["row_count"].is_null(),
            "virtual tables are never counted"
        );
        assert_eq!(r["row_counts_complete"], false);
        assert_eq!(r["integrity_check_kind"], "quick_check");
    }

    #[test]
    fn quote_sqlite_identifier_doubles_embedded_quotes() {
        assert_eq!(quote_sqlite_identifier("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn select_all_rows() {
        let (_f, path) = create_test_db();
        let result = query(&path, "SELECT * FROM users", &[], None).unwrap();
        assert_eq!(result["row_count"], 3);
        assert_eq!(
            result["columns"],
            serde_json::json!(["id", "name", "score"])
        );
        assert_eq!(result["rows"][0]["name"], "Alice");
        assert_eq!(result["rows"][1]["name"], "Bob");
    }

    #[test]
    fn select_with_params() {
        let (_f, path) = create_test_db();
        let result = query(
            &path,
            "SELECT name FROM users WHERE score > ?",
            &[serde_json::json!(90.0)],
            None,
        )
        .unwrap();
        assert_eq!(result["row_count"], 2);
    }

    #[test]
    fn max_rows_truncation() {
        let (_f, path) = create_test_db();
        let result = query(&path, "SELECT * FROM users", &[], Some(2)).unwrap();
        assert_eq!(result["row_count"], 2);
        assert_eq!(result["truncated"], true);
    }

    #[test]
    fn exact_max_rows_is_not_truncated() {
        let (_f, path) = create_test_db();
        let result = query(&path, "SELECT * FROM users", &[], Some(3)).unwrap();
        assert_eq!(result["row_count"], 3);
        assert_eq!(result["truncated"], false);
    }

    #[test]
    fn result_byte_limit_truncates_before_unbounded_allocation() {
        let file = tempfile::NamedTempFile::with_suffix(".sqlite").unwrap();
        let path = file.path().to_path_buf();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE payloads (data TEXT)")
            .unwrap();
        let payload = "x".repeat(120_000);
        for _ in 0..5 {
            conn.execute("INSERT INTO payloads VALUES (?)", [&payload])
                .unwrap();
        }

        let result = query_with_limits(
            &path,
            "SELECT data FROM payloads",
            &[],
            None,
            QUERY_TIMEOUT,
            250_000,
        )
        .unwrap();
        assert_eq!(result["row_count"], 2);
        assert_eq!(result["truncated"], true);
        assert!(result["result_bytes"].as_u64().unwrap() <= 250_000);
    }

    /// Audit F1: `SQLITE_LIMIT_COLUMN` was the 2000 default, so one row of 1 MB cells could
    /// reach gigabytes inside `SQLite` + the host before any budget was consulted.
    #[test]
    fn result_set_width_is_capped() {
        let (_f, path) = create_test_db();
        let cols = (1..=300)
            .map(|i| format!("x AS c{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("WITH b(x) AS (SELECT 1) SELECT {cols} FROM b");
        let err = query(&path, &sql, &[], None).unwrap_err();
        assert!(err.contains("too many columns"), "{err}");
    }

    /// The column cap limits result sets, never the ability to read an app's wide table.
    #[test]
    fn wide_table_is_still_readable_under_the_column_cap() {
        let file = tempfile::NamedTempFile::with_suffix(".sqlite").unwrap();
        let conn = rusqlite::Connection::open(file.path()).unwrap();
        let cols = (1..=300)
            .map(|i| format!("c{i} INTEGER"))
            .collect::<Vec<_>>()
            .join(", ");
        conn.execute_batch(&format!(
            "CREATE TABLE wide ({cols}); INSERT INTO wide (c1, c300) VALUES (7, 9);"
        ))
        .unwrap();
        drop(conn);
        let r = query(file.path(), "SELECT c1, c300 FROM wide", &[], None).unwrap();
        assert_eq!(r["rows"][0]["c1"], 7);
        assert_eq!(r["rows"][0]["c300"], 9);
        // Only a result set that wide is refused, with a clear reason.
        let err = query(file.path(), "SELECT * FROM wide", &[], None).unwrap_err();
        assert!(err.contains("too many columns"), "{err}");
    }

    /// Audit F1: a row of many large blobs stops at the byte cap, and what IS returned is
    /// within the cap (the budget is charged per cell, before conversion).
    #[test]
    fn wide_row_of_large_blobs_stops_at_the_byte_cap() {
        let (_f, path) = create_test_db();
        let cols = (1..=40)
            .map(|i| format!("x AS c{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("WITH b(x) AS (SELECT zeroblob(1000000)) SELECT {cols} FROM b");
        let r = query(&path, &sql, &[], None).unwrap();
        assert_eq!(r["truncated"], true);
        assert_eq!(r["row_count"], 0);
        assert!(r["result_bytes"].as_u64().unwrap() <= MAX_QUERY_RESULT_BYTES as u64);
    }

    /// `result_bytes` is the exact encoded size of `columns` + every returned row, measured
    /// from raw cells without re-serializing the row.
    #[test]
    fn result_bytes_matches_the_encoded_rows_exactly() {
        let (_f, path) = create_test_db();
        let r = query(
            &path,
            "SELECT id, name, score, NULL AS n, X'00FF10' AS b, '{\"k\": [1, 2]}' AS j, \
             'q\"\\\n\u{1}é' AS s, 1.5e300 AS big FROM users",
            &[],
            None,
        )
        .unwrap();
        let mut expected = serde_json::to_vec(&r["columns"]).unwrap().len();
        for row in r["rows"].as_array().unwrap() {
            expected += serde_json::to_vec(row).unwrap().len();
        }
        assert_eq!(r["result_bytes"].as_u64().unwrap() as usize, expected);
        assert!(r["rows"][0]["j"].is_object());
        assert_eq!(r["rows"][0]["b"]["size"], 3);
    }

    /// Audit F1: `SELECT 1 AS a, 2 AS a` returned `columns: [a, a]` but a row object with one
    /// `a` — a value silently vanished.
    #[test]
    fn duplicate_column_names_keep_every_value() {
        let (_f, path) = create_test_db();
        let r = query(
            &path,
            "SELECT 1 AS a, 2 AS a, 3 AS \"a:1\", 4 AS a",
            &[],
            None,
        )
        .unwrap();
        assert_eq!(r["columns"], serde_json::json!(["a", "a:2", "a:1", "a:3"]));
        let row = &r["rows"][0];
        assert_eq!(row["a"], 1);
        assert_eq!(row["a:2"], 2);
        assert_eq!(row["a:1"], 3);
        assert_eq!(row["a:3"], 4);
        assert_eq!(row.as_object().unwrap().len(), 4);
    }

    /// Audit F2: `LIKE`/`GLOB` never check for an interrupt, so a long pattern ran tens of
    /// seconds past the deadline. Long patterns are now refused up front.
    #[test]
    fn long_like_and_glob_patterns_are_refused() {
        let (_f, path) = create_test_db();
        let pattern = format!("%{}%", "a".repeat(5_000));
        for sql in [
            "SELECT name FROM users WHERE name LIKE ?",
            "SELECT name FROM users WHERE name GLOB ?",
        ] {
            let started = Instant::now();
            let err = query(&path, sql, &[serde_json::json!(pattern)], None).unwrap_err();
            assert!(err.contains("pattern too complex"), "{sql}: {err}");
            // Refused per row BEFORE matching (generous bound: loaded CI machines).
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{:?}",
                started.elapsed()
            );
        }
        // An ordinary pattern still works.
        let r = query(
            &path,
            "SELECT name FROM users WHERE name LIKE ?",
            &[serde_json::json!("A%")],
            None,
        )
        .unwrap();
        assert_eq!(r["row_count"], 1);
    }

    /// Audit F7: a query waited up to 5s on the app's lock before its 5s CPU deadline even
    /// started, holding a read transaction (and stalling WAL checkpoints) for ~10s.
    #[test]
    fn lock_wait_is_short() {
        let (_f, path) = create_test_db();
        let locker = rusqlite::Connection::open(&path).unwrap();
        locker.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let started = Instant::now();
        let err = query(&path, "SELECT * FROM users", &[], None).unwrap_err();
        assert!(err.contains("locked"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "lock wait took {:?}",
            started.elapsed()
        );
        locker.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn cpu_heavy_query_times_out() {
        let (_f, path) = create_test_db();
        let err = query_with_limits(
            &path,
            "WITH RECURSIVE cnt(x) AS (VALUES(1) UNION ALL SELECT x + 1 FROM cnt) \
             SELECT sum(x) FROM cnt",
            &[],
            None,
            Duration::from_millis(1),
            MAX_QUERY_RESULT_BYTES,
        )
        .unwrap_err();
        assert!(err.contains("timed out"), "unexpected timeout error: {err}");
    }

    #[test]
    fn oversized_sql_is_rejected_before_prepare() {
        let (_f, path) = create_test_db();
        let sql = format!("SELECT 1 /*{}*/", "x".repeat(MAX_QUERY_SQL_BYTES));
        let err = query(&path, &sql, &[], None).unwrap_err();
        assert!(err.contains("maximum length"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_insert() {
        let (_f, path) = create_test_db();
        let err = query(
            &path,
            "INSERT INTO users VALUES (4, 'Eve', 99.0)",
            &[],
            None,
        )
        .unwrap_err();
        assert!(err.contains("read-only"));
    }

    #[test]
    fn rejects_delete() {
        let (_f, path) = create_test_db();
        let err = query(&path, "DELETE FROM users", &[], None).unwrap_err();
        assert!(err.contains("read-only"));
    }

    #[test]
    fn rejects_drop() {
        let (_f, path) = create_test_db();
        let err = query(&path, "DROP TABLE users", &[], None).unwrap_err();
        assert!(err.contains("read-only"));
    }

    #[test]
    fn rejects_update() {
        let (_f, path) = create_test_db();
        let err = query(&path, "UPDATE users SET name = 'X'", &[], None).unwrap_err();
        assert!(err.contains("read-only"));
    }

    #[test]
    fn pragma_works() {
        let (_f, path) = create_test_db();
        let result = query(&path, "PRAGMA table_info(users)", &[], None).unwrap();
        assert!(result["row_count"].as_u64().unwrap() >= 3);
    }

    #[test]
    fn pragma_read_allowed() {
        let (_f, path) = create_test_db();
        assert!(query(&path, "PRAGMA journal_mode", &[], None).is_ok());
        assert!(query(&path, "PRAGMA user_version", &[], None).is_ok());
    }

    #[test]
    fn rejects_side_effecting_pragmas() {
        // Audit C10: side-effecting PRAGMAs without `=` are blocked by the allowlist.
        let (_f, path) = create_test_db();
        for sql in [
            "PRAGMA wal_checkpoint",
            "PRAGMA wal_checkpoint(TRUNCATE)",
            "PRAGMA optimize",
            "PRAGMA incremental_vacuum",
            "PRAGMA shrink_memory",
            "PRAGMA main.wal_checkpoint",
            "pragma  optimize ",
        ] {
            let err = query(&path, sql, &[], None).unwrap_err();
            assert!(
                err.contains("read-only introspection PRAGMAs"),
                "expected allowlist block for: {sql} (got: {err})"
            );
        }
    }

    #[test]
    fn allows_safe_introspection_pragmas() {
        let (_f, path) = create_test_db();
        for sql in [
            "PRAGMA table_info(users)",
            "PRAGMA integrity_check",
            "PRAGMA page_count",
            "PRAGMA foreign_key_list(users)",
            "PRAGMA main.table_info(users)",
        ] {
            assert!(
                query(&path, sql, &[], None).is_ok(),
                "expected ok for: {sql}"
            );
        }
    }

    #[test]
    fn pragma_name_handles_schema_qualifier_and_args() {
        assert_eq!(
            pragma_name("PRAGMA wal_checkpoint").as_deref(),
            Some("wal_checkpoint")
        );
        assert_eq!(
            pragma_name("PRAGMA main.table_info(users)").as_deref(),
            Some("table_info")
        );
        assert_eq!(
            pragma_name("PRAGMA table_info(users)").as_deref(),
            Some("table_info")
        );
        // Quoted/bracketed schema qualifiers normalize to the same name (no false block).
        assert_eq!(
            pragma_name(r#"PRAGMA "main".table_info(users)"#).as_deref(),
            Some("table_info")
        );
        assert_eq!(
            pragma_name("PRAGMA [main].wal_checkpoint").as_deref(),
            Some("wal_checkpoint")
        );
        assert_eq!(pragma_name("SELECT 1"), None);
    }

    #[test]
    fn quoted_schema_read_pragma_is_allowed_but_quoted_side_effect_blocked() {
        let (_f, path) = create_test_db();
        // A legitimate quoted-schema read PRAGMA must not be falsely blocked.
        assert!(query(&path, r#"PRAGMA "main".table_info(users)"#, &[], None).is_ok());
        // …but a side-effecting one stays blocked even when quoted.
        let err = query(&path, "PRAGMA [main].wal_checkpoint", &[], None).unwrap_err();
        assert!(err.contains("read-only introspection PRAGMAs"));
    }

    #[test]
    fn rejects_pragma_write_form() {
        let (_f, path) = create_test_db();
        for sql in [
            "PRAGMA journal_mode=DELETE",
            "PRAGMA journal_mode = WAL",
            "PRAGMA user_version=12345",
            "  pragma  synchronous = 0 ",
        ] {
            let err = query(&path, sql, &[], None).unwrap_err();
            assert!(err.contains("PRAGMA writes"), "expected block for: {sql}");
        }
    }

    #[test]
    fn is_pragma_write_ignores_equals_in_strings() {
        // A read-form PRAGMA whose argument contains '=' inside quotes is not a write.
        assert!(!is_pragma_write("PRAGMA table_info('a=b')"));
        assert!(is_pragma_write("PRAGMA foo = 'a=b'"));
        assert!(!is_pragma_write("SELECT 1 = 1"));
    }

    #[test]
    fn with_cte_works() {
        let (_f, path) = create_test_db();
        let result = query(
            &path,
            "WITH top AS (SELECT * FROM users WHERE score > 90) SELECT name FROM top",
            &[],
            None,
        )
        .unwrap();
        assert_eq!(result["row_count"], 2);
    }

    #[test]
    fn nonexistent_db_fails() {
        let err = query(Path::new("/nonexistent/db.sqlite"), "SELECT 1", &[], None).unwrap_err();
        assert!(err.contains("failed to open"));
    }

    #[test]
    fn json_column_parsed() {
        let file = tempfile::NamedTempFile::with_suffix(".sqlite").unwrap();
        let path = file.path().to_path_buf();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"CREATE TABLE config (key TEXT, value TEXT);
               INSERT INTO config VALUES ('settings', '{"theme":"dark","lang":"en"}');"#,
        )
        .unwrap();
        let result = query(&path, "SELECT * FROM config", &[], None).unwrap();
        assert!(result["rows"][0]["value"].is_object());
        assert_eq!(result["rows"][0]["value"]["theme"], "dark");
    }

    #[test]
    fn discover_finds_sqlite_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::File::create(dir.path().join("app.sqlite")).unwrap();
        std::fs::File::create(dir.path().join("cache.db")).unwrap();
        std::fs::File::create(dir.path().join("readme.txt")).unwrap();
        let sub = dir.path().join("subdir");
        std::fs::create_dir(&sub).unwrap();
        std::fs::File::create(sub.join("deep.sqlite3")).unwrap();

        let dbs = discover_databases(dir.path());
        assert_eq!(dbs.len(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn discover_does_not_follow_directory_symlink_outside_root() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::File::create(outside.path().join("outside.db")).unwrap();
        symlink(outside.path(), dir.path().join("escape")).unwrap();

        assert!(discover_databases(dir.path()).is_empty());
    }

    #[test]
    fn rejects_comment_bypass_block() {
        let (_f, path) = create_test_db();
        let err = query(&path, "/* sneaky */DELETE FROM users", &[], None).unwrap_err();
        assert!(err.contains("read-only"));
    }

    #[test]
    fn rejects_line_comment_bypass() {
        let (_f, path) = create_test_db();
        let err = query(&path, "-- comment\nDELETE FROM users", &[], None).unwrap_err();
        assert!(err.contains("read-only"));
    }

    #[test]
    fn rejects_stacked_queries() {
        let (_f, path) = create_test_db();
        let err = query(&path, "SELECT 1; DROP TABLE users", &[], None).unwrap_err();
        assert!(err.contains("stacked queries"));
    }

    #[test]
    fn allows_trailing_semicolon() {
        let (_f, path) = create_test_db();
        let result = query(&path, "SELECT * FROM users;", &[], None).unwrap();
        assert_eq!(result["row_count"], 3);
    }

    #[test]
    fn allows_select_with_block_comment() {
        let (_f, path) = create_test_db();
        let result = query(
            &path,
            "/* filter */ SELECT name FROM users WHERE id = 1",
            &[],
            None,
        )
        .unwrap();
        assert_eq!(result["row_count"], 1);
        assert_eq!(result["rows"][0]["name"], "Alice");
    }

    #[test]
    fn rejects_empty_query() {
        let (_f, path) = create_test_db();
        let err = query(&path, "", &[], None).unwrap_err();
        assert!(err.contains("read-only"));
    }

    #[test]
    fn rejects_comment_only_query() {
        let (_f, path) = create_test_db();
        let err = query(&path, "/* just a comment */", &[], None).unwrap_err();
        assert!(err.contains("read-only"));
    }

    #[test]
    fn rejects_nested_comment_bypass() {
        let (_f, path) = create_test_db();
        let err = query(
            &path,
            "/* outer /* inner */ still comment */ DROP TABLE users",
            &[],
            None,
        )
        .unwrap_err();
        assert!(err.contains("read-only"));
    }

    #[test]
    fn blob_column_base64() {
        let file = tempfile::NamedTempFile::with_suffix(".sqlite").unwrap();
        let path = file.path().to_path_buf();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE blobs (id INTEGER, data BLOB)")
            .unwrap();
        conn.execute("INSERT INTO blobs VALUES (1, X'DEADBEEF')", [])
            .unwrap();
        let result = query(&path, "SELECT * FROM blobs", &[], None).unwrap();
        assert!(result["rows"][0]["data"]["__blob"].as_bool().unwrap());
        assert_eq!(result["rows"][0]["data"]["size"], 4);
    }

    // ── WebView-internal exclusion + app-DB selection (audit / red-team "wrong DB") ──

    fn write_sqlite(path: &Path, rows: usize) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, blob TEXT)")
            .unwrap();
        for i in 0..rows {
            conn.execute("INSERT INTO t (blob) VALUES (?)", [format!("row-{i}")])
                .unwrap();
        }
    }

    #[test]
    fn flags_webview_internal_stores() {
        assert!(is_webview_internal(Path::new(
            "/app/EBWebView/Default/Cookies"
        )));
        assert!(is_webview_internal(Path::new(
            "/app/EBWebView/Default/QuotaManager"
        )));
        assert!(is_webview_internal(Path::new(
            "/Users/x/Library/WebKit/IndexedDB/file__0.indexeddb.sqlite3"
        )));
        assert!(is_webview_internal(Path::new(
            "/app/Local Storage/leveldb.db"
        )));
        assert!(is_webview_internal(Path::new("/app/data/web data")));
        // Real application DBs are NOT flagged.
        assert!(!is_webview_internal(Path::new("/app/data/4da.db")));
        assert!(!is_webview_internal(Path::new("/app/data/app.sqlite")));
        assert!(!is_webview_internal(Path::new("/app/notes.db")));
    }

    #[test]
    fn selects_app_db_over_webview_internals() {
        // Reproduces the red-team layout: a WebView profile dir full of engine SQLite
        // files sitting next to the real (larger) application DB. The selector must pick
        // the app DB, never Cookies/QuotaManager.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Engine internals: flagged either by basename (Cookies/QuotaManager) or by living
        // under a WebView profile dir (EBWebView). Real Chromium files are extensionless
        // (and thus skipped by the extension filter entirely); we give them a recognized
        // extension here precisely to prove the denylist also catches the extensioned forms
        // (e.g. WebKit `*.indexeddb.sqlite3`).
        write_sqlite(&root.join("EBWebView/Default/Cookies.db"), 1);
        write_sqlite(&root.join("EBWebView/Default/QuotaManager.sqlite"), 1);
        write_sqlite(&root.join("app.sqlite"), 200); // the real app DB (largest)

        let selected = select_app_database(&[root.to_path_buf()]).unwrap();
        assert_eq!(selected.file_name().unwrap(), "app.sqlite");

        let classified = classify_databases(&[root.to_path_buf()]);
        assert!(!classified[0].webview_internal, "app DB must rank first");
        assert_eq!(classified[0].path.file_name().unwrap(), "app.sqlite");
        assert!(
            classified.iter().filter(|c| c.webview_internal).count() >= 2,
            "Cookies + QuotaManager must be tagged as internal"
        );
    }

    #[test]
    fn errors_clearly_when_only_webview_internals_present() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_sqlite(&root.join("EBWebView/Default/Cookies.db"), 1);
        write_sqlite(&root.join("EBWebView/Default/QuotaManager.sqlite"), 1);

        let err = select_app_database(&[root.to_path_buf()]).unwrap_err();
        assert!(
            err.contains("WebView") && err.contains("db_search_paths"),
            "error should name the cause and the fix: {err}"
        );
    }

    #[test]
    fn larger_app_db_outranks_smaller_one() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_sqlite(&root.join("small.db"), 1);
        write_sqlite(&root.join("big.db"), 500);
        let selected = select_app_database(&[root.to_path_buf()]).unwrap();
        assert_eq!(selected.file_name().unwrap(), "big.db");
    }
}
