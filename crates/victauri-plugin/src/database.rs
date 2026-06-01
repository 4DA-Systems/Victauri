#[cfg(feature = "sqlite")]
use std::path::{Path, PathBuf};

#[cfg(feature = "sqlite")]
const MAX_ROWS_DEFAULT: usize = 100;
#[cfg(feature = "sqlite")]
const MAX_ROWS_LIMIT: usize = 10_000;

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

/// Discover `SQLite` database files in a directory (non-recursive, max depth 2).
#[cfg(feature = "sqlite")]
#[must_use]
pub fn discover_databases(dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    discover_recursive(dir, 0, 2, &mut results);
    results
}

#[cfg(feature = "sqlite")]
fn discover_recursive(dir: &Path, depth: u32, max_depth: u32, results: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() {
            continue;
        }
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str())
                && matches!(ext, "sqlite" | "sqlite3" | "db" | "sdb")
            {
                results.push(path);
            }
        } else if path.is_dir() && depth < max_depth {
            discover_recursive(&path, depth + 1, max_depth, results);
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

    // 5 second query timeout
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| format!("failed to set timeout: {e}"))?;

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("failed to prepare query: {e}"))?;

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
        .map_err(|e| format!("query execution failed: {e}"))?;

    while let Some(row) = rows.next().map_err(|e| format!("row read failed: {e}"))? {
        if rows_out.len() >= max_rows {
            break;
        }
        let mut obj = serde_json::Map::new();
        for (i, col_name) in column_names.iter().enumerate().take(column_count) {
            let value = row_value_to_json(row, i);
            obj.insert(col_name.clone(), value);
        }
        rows_out.push(serde_json::Value::Object(obj));
    }

    let truncated = rows_out.len() == max_rows;

    Ok(serde_json::json!({
        "columns": column_names,
        "rows": rows_out,
        "row_count": rows_out.len(),
        "truncated": truncated,
        "max_rows": max_rows,
    }))
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
