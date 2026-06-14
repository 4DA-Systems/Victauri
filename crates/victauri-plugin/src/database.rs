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
        let poll = Duration::from_millis(25).min(deadline.max(Duration::from_millis(1)));
        let handle = std::thread::spawn(move || {
            let start = Instant::now();
            while start.elapsed() < deadline {
                if done_for_thread.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(poll);
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
            let _ = h.join();
        }
    }
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
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(feature = "sqlite")]
const QUERY_PROGRESS_OPS: i32 = 10_000;

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

#[cfg(feature = "sqlite")]
fn query_with_limits(
    db_path: &Path,
    sql: &str,
    params: &[serde_json::Value],
    max_rows: Option<usize>,
    query_timeout: Duration,
    max_result_bytes: usize,
) -> Result<serde_json::Value, String> {
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

    let max_rows = max_rows.unwrap_or(MAX_ROWS_DEFAULT).min(MAX_ROWS_LIMIT);

    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("failed to open database: {e}"))?;

    // Limit lock waits separately from the CPU deadline enforced below.
    conn.busy_timeout(std::time::Duration::from_secs(5))
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
    let started = Instant::now();
    conn.progress_handler(
        QUERY_PROGRESS_OPS,
        Some(move || started.elapsed() >= query_timeout),
    );
    // Hard wall-clock backstop: the progress handler under-samples a single long op, so also
    // arm an interrupt-handle watchdog. Lives until the query completes (drops/joins on every
    // return path, including `?` errors below).
    let _interrupt = InterruptGuard::arm(&conn, query_timeout);

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| sqlite_query_error("failed to prepare query", e, query_timeout))?;

    let column_names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let column_count = column_names.len();

    let sqlite_params: Vec<Box<dyn rusqlite::types::ToSql>> =
        params.iter().map(json_to_sql).collect();
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = sqlite_params.iter().map(|b| &**b).collect();

    let mut rows_out: Vec<serde_json::Value> = Vec::new();
    let mut rows = stmt
        .query(param_refs.as_slice())
        .map_err(|e| sqlite_query_error("query execution failed", e, query_timeout))?;
    let mut result_bytes = serde_json::to_vec(&column_names)
        .map_err(|e| format!("failed to size query result columns: {e}"))?
        .len();
    let mut truncated = false;

    while let Some(row) = rows
        .next()
        .map_err(|e| sqlite_query_error("row read failed", e, query_timeout))?
    {
        if rows_out.len() >= max_rows {
            truncated = true;
            break;
        }
        let mut obj = serde_json::Map::new();
        for (i, col_name) in column_names.iter().enumerate().take(column_count) {
            let value = row_value_to_json(row, i);
            obj.insert(col_name.clone(), value);
        }
        let row_value = serde_json::Value::Object(obj);
        let row_bytes = serde_json::to_vec(&row_value)
            .map_err(|e| format!("failed to size query result row: {e}"))?
            .len();
        if result_bytes.saturating_add(row_bytes) > max_result_bytes {
            truncated = true;
            break;
        }
        result_bytes = result_bytes.saturating_add(row_bytes);
        rows_out.push(row_value);
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

#[cfg(feature = "sqlite")]
fn row_value_to_json(row: &rusqlite::Row, idx: usize) -> serde_json::Value {
    use rusqlite::types::ValueRef;
    match row.get_ref(idx) {
        Ok(ValueRef::Null) => serde_json::Value::Null,
        Ok(ValueRef::Integer(i)) => serde_json::json!(i),
        Ok(ValueRef::Real(f)) => serde_json::json!(f),
        Ok(ValueRef::Text(t)) => {
            let s = String::from_utf8_lossy(t);
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&s)
                && (parsed.is_object() || parsed.is_array())
            {
                return parsed;
            }
            serde_json::Value::String(s.into_owned())
        }
        Ok(ValueRef::Blob(b)) => {
            use base64::Engine;
            serde_json::json!({
                "__blob": true,
                "size": b.len(),
                "base64": base64::engine::general_purpose::STANDARD.encode(b),
            })
        }
        Err(_) => serde_json::Value::Null,
    }
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
