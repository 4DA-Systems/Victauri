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

/// Victauri accepts a RANGE of rusqlite versions (`>=0.32, <0.41`) so it unifies with the app's
/// own rusqlite instead of forcing a second `libsqlite3-sys` (`links = "sqlite3"` allows only
/// one per build). Across that range several connection setters changed return type — `()` or
/// `i32` on older versions, `rusqlite::Result` on newer ones (`set_limit` from 0.33,
/// `authorizer`/`progress_handler` from 0.38). They install SECURITY bounds (the authorizer,
/// deadlines, size limits), so a failure must fail CLOSED on every version: this normalizes all
/// three shapes into one `Result` for `?`, instead of a version-dependent `let _ =` that would
/// silently drop a failed install.
#[cfg(feature = "sqlite")]
trait SetupOutcome {
    fn into_setup(self) -> Result<(), String>;
}

#[cfg(feature = "sqlite")]
impl SetupOutcome for () {
    fn into_setup(self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
impl SetupOutcome for i32 {
    fn into_setup(self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
impl<T> SetupOutcome for rusqlite::Result<T> {
    fn into_setup(self) -> Result<(), String> {
        self.map(|_| ()).map_err(|e| e.to_string())
    }
}

/// Run `f` under its OWN wall-clock budget: a progress handler plus a hard [`InterruptGuard`],
/// both scoped to this call and removed afterwards, so one slow phase cannot poison the phases
/// that follow it on the same connection.
#[cfg(feature = "sqlite")]
// `unit_arg`: on rusqlite 0.32 some setters return `()`; `.into_setup()` on that unit is the
// point (see `SetupOutcome`), so one source compiles fail-closed across the supported range.
#[allow(clippy::unit_arg)]
pub(crate) fn run_bounded<T>(
    conn: &rusqlite::Connection,
    budget: Duration,
    f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
) -> Bounded<T> {
    let started = Instant::now();
    let timed_out = Arc::new(AtomicBool::new(false));
    let marker = Arc::clone(&timed_out);
    // The per-phase deadline is a security bound: if it cannot be installed, the phase fails
    // rather than running unbounded.
    if let Err(e) = conn
        .progress_handler(
            DB_HEALTH_PROGRESS_OPS,
            Some(move || {
                let expired = started.elapsed() >= budget;
                if expired {
                    marker.store(true, Ordering::Relaxed);
                }
                expired
            }),
        )
        .into_setup()
    {
        return Bounded::Failed(format!("failed to install the phase deadline: {e}"));
    }
    let result = {
        let _interrupt = InterruptGuard::arm(conn, budget);
        f(conn)
    };
    // Removing it can only fail harmlessly (the next phase installs its own deadline).
    let _ = conn
        .progress_handler(DB_HEALTH_PROGRESS_OPS, None::<fn() -> bool>)
        .into_setup();
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
/// counted (counting one runs its module's code). That does not keep virtual-table module code
/// out of the call by itself: a whole-database `quick_check` connects every virtual table whose
/// module is registered and (`SQLite` >= 3.44) runs its `xIntegrity` check. Victauri registers no
/// module on this connection, so that is `SQLite`'s built-ins (FTS3/4/5, R-Tree — checked: on
/// 3.46 a corrupted FTS5 index reports `malformed inverted index for FTS5 table …`) plus
/// anything the host process installed as an auto-extension. So when the host registered a
/// non-built-in module and the file has virtual tables, the check runs per ordinary table
/// instead (`integrity_check_kind: "quick_check (per table)"`, with the reason in
/// `integrity_check_note`) and no module code runs.
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
    )
    .into_setup()
    .map_err(|e| format!("failed to harden database connection: {e}"))?;
    // The metadata phase runs `LIKE` against schema SQL the database file supplies.
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH,
        MAX_LIKE_PATTERN_BYTES,
    )
    .into_setup()
    .map_err(|e| format!("failed to harden database connection: {e}"))?;

    // Phase 0: metadata + the (bounded) table listing.
    let meta = run_bounded(&conn, DB_HEALTH_META_BUDGET, |c| {
        let text = |name: &str| c.pragma_query_value(None, name, |r| r.get::<_, String>(0));
        let int = |name: &str| c.pragma_query_value(None, name, |r| r.get::<_, i64>(0));
        let journal_mode = text("journal_mode")?;
        let page_count = int("page_count")?;
        let page_size = int("page_size")?;
        let freelist_count = int("freelist_count")?;
        // Only a table whose definition is exactly SQLite's own `CREATE TABLE ` form is counted.
        // SQLite writes every definition in canonical form (`CREATE TABLE <name>…` /
        // `CREATE VIRTUAL TABLE <name>…`, one space each), so a legitimate file never differs;
        // a crafted one can (via `writable_schema`) spell a virtual table `CREATE  VIRTUAL`,
        // `CREATE/**/VIRTUAL`, … which a `LIKE 'CREATE VIRTUAL%'` test missed — and then
        // `count(*)` ran the module's code. Anything not in canonical ordinary-table form is
        // listed, never counted.
        let mut stmt = c.prepare(
            "SELECT name, substr(sql, 1, 13) IS 'CREATE TABLE ', \
             instr(upper(substr(sql, 1, 256)), 'VIRTUAL') > 0 \
             FROM sqlite_master WHERE type='table' ORDER BY name",
        )?;
        let mut names: Vec<(String, TableKind)> = Vec::new();
        let mut table_bytes = 0usize;
        let mut truncated = false;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            let kind = if row.get::<_, Option<bool>>(1)?.unwrap_or(false) {
                TableKind::Ordinary
            } else if row.get::<_, Option<bool>>(2)?.unwrap_or(false) {
                TableKind::Virtual
            } else {
                TableKind::Unrecognized
            };
            if names.len() >= MAX_DB_HEALTH_TABLES
                || table_bytes.saturating_add(name.len()) > MAX_DB_HEALTH_TABLE_BYTES
            {
                truncated = true;
                break;
            }
            table_bytes = table_bytes.saturating_add(name.len());
            names.push((name, kind));
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

    // Needed by phase 2 (the loop below consumes `names`).
    let ordinary_tables: Vec<String> = names
        .iter()
        .filter(|(_, kind)| *kind == TableKind::Ordinary)
        .map(|(name, _)| name.clone())
        .collect();
    let may_hold_virtual_tables =
        tables_truncated || names.iter().any(|(_, kind)| *kind != TableKind::Ordinary);

    // Phase 1: per-table row counts under a shared count budget.
    let counts_started = Instant::now();
    let mut budget_exhausted = false;
    let mut all_counted = true;
    let mut tables = Vec::with_capacity(names.len());
    for (name, kind) in names {
        let mut entry = serde_json::json!({ "name": name, "row_count": null });
        if kind == TableKind::Virtual {
            entry["virtual"] = serde_json::json!(true);
            all_counted = false;
        } else if kind == TableKind::Unrecognized {
            entry["count_skipped"] =
                serde_json::json!("definition is not in SQLite's canonical CREATE TABLE form");
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

    // Phase 2: integrity, on its own budget. A whole-database `quick_check` connects every
    // virtual table whose module is registered and (SQLite >= 3.44) runs the module's integrity
    // routine. SQLite's own modules are fine; a module the HOST registered process-wide (an
    // auto-extension) is arbitrary code. With such a module registered and virtual tables in
    // the file, check the ordinary tables one by one (`PRAGMA quick_check(<table>)`), which
    // connects no virtual table.
    let per_table_reason = if may_hold_virtual_tables {
        match foreign_vtab_modules(&conn) {
            Ok(modules) if modules.is_empty() => None,
            Ok(modules) => Some(format!(
                "the database has virtual tables and the host registered non-built-in \
                 virtual-table module(s) {}",
                modules.join(", ")
            )),
            Err(e) => Some(format!(
                "the database has virtual tables and the registered virtual-table modules could \
                 not be listed ({e})"
            )),
        }
    } else {
        None
    };
    let (integrity, integrity_kind, integrity_note) = match per_table_reason {
        None => {
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
            (integrity, "quick_check", None)
        }
        Some(reason) => {
            let integrity = match run_bounded(&conn, check_budget, |c| {
                quick_check_tables(c, &ordinary_tables)
            }) {
                Bounded::Done(problems) if problems.is_empty() => "ok".to_string(),
                Bounded::Done(problems) => problems.join("\n"),
                Bounded::TimedOut => format!(
                    "not completed: the per-table quick_check exceeded its {} ms budget on a \
                     {:.0} MB database (the result is unknown, not failed)",
                    check_budget.as_millis(),
                    db_size_mb
                ),
                Bounded::Failed(e) => format!("failed: {e}"),
            };
            let note = format!(
                "{reason}: checked the {} ordinary table(s) one by one instead of the whole \
                 database, so no virtual-table module code ran. Virtual tables and \
                 database-wide structures (the free list) were not checked.",
                ordinary_tables.len()
            );
            (integrity, "quick_check (per table)", Some(note))
        }
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
        "integrity_check_kind": integrity_kind,
        "integrity_check_note": integrity_note,
        "tables": tables,
        "tables_truncated": tables_truncated,
        // Every listed table has a row count (false if the budget ran out, a count failed, or a
        // table is virtual and deliberately not counted).
        "row_counts_complete": all_counted,
        "row_count_budget_exhausted": budget_exhausted,
    }))
}

/// Virtual-table modules that are part of `SQLite` itself (compiled-in extensions and the
/// eponymous introspection tables). Anything else registered on a connection came from the
/// host process.
#[cfg(feature = "sqlite")]
static SQLITE_BUILTIN_VTAB_MODULES: &[&str] = &[
    "fts3",
    "fts3tokenize",
    "fts4",
    "fts4aux",
    "fts5",
    "fts5vocab",
    "rtree",
    "rtree_i32",
    "geopoly",
    "dbstat",
    "sqlite_dbpage",
    "sqlite_dbdata",
    "sqlite_dbptr",
    "sqlite_stmt",
    "json_each",
    "json_tree",
    "jsonb_each",
    "jsonb_tree",
    "carray",
    "generate_series",
    "bytecode",
    "tables_used",
    "completion",
];

/// Most problems reported by a per-table `quick_check`.
#[cfg(feature = "sqlite")]
const MAX_INTEGRITY_PROBLEMS: usize = 100;

/// The virtual-table modules registered on `conn` that are not part of `SQLite`.
#[cfg(feature = "sqlite")]
fn foreign_vtab_modules(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_module_list ORDER BY name")?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names
        .into_iter()
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            !lower.starts_with("pragma_") && !SQLITE_BUILTIN_VTAB_MODULES.contains(&lower.as_str())
        })
        .collect())
}

/// `PRAGMA quick_check(<table>)` for each of `tables` (ordinary tables only: checking one
/// connects no virtual table). Returns the problems found (empty = ok), at most
/// [`MAX_INTEGRITY_PROBLEMS`].
#[cfg(feature = "sqlite")]
fn quick_check_tables(
    conn: &rusqlite::Connection,
    tables: &[String],
) -> rusqlite::Result<Vec<String>> {
    let mut problems = Vec::new();
    for table in tables {
        let sql = format!("PRAGMA quick_check({})", quote_sqlite_identifier(table));
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let line: String = row.get(0)?;
            if line != "ok" && problems.len() < MAX_INTEGRITY_PROBLEMS {
                problems.push(line);
            }
        }
    }
    Ok(problems)
}

/// How [`db_health_report`] treats a `sqlite_master` table entry.
#[cfg(feature = "sqlite")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum TableKind {
    /// Canonical `CREATE TABLE ` definition: counted.
    Ordinary,
    /// A virtual table (by any spelling): listed, not counted.
    Virtual,
    /// Neither: listed, not counted.
    Unrecognized,
}

/// Budget for the metadata PRAGMAs + table listing in [`db_health_report`].
#[cfg(feature = "sqlite")]
pub(crate) const DB_HEALTH_META_BUDGET: Duration = Duration::from_secs(3);

/// A hook run on each connection Victauri opens (tests only).
#[cfg(all(test, feature = "sqlite"))]
type OpenHook = Box<dyn Fn(&rusqlite::Connection)>;

#[cfg(all(test, feature = "sqlite"))]
thread_local! {
    /// Test-only stand-in for what a host process can do to every connection `SQLite` opens
    /// (`sqlite3_auto_extension`): register functions or virtual-table modules on it. Run right
    /// after each of Victauri's connections is opened.
    static ON_OPEN: std::cell::RefCell<Option<OpenHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(feature = "sqlite")]
fn run_open_hook(_conn: &rusqlite::Connection) {
    #[cfg(test)]
    ON_OPEN.with(|hook| {
        if let Some(hook) = hook.borrow().as_ref() {
            hook(_conn);
        }
    });
}

/// Open a database file Victauri did not create as read-only UNTRUSTED input: with
/// `trusted_schema=OFF` (schema-embedded SQL functions / virtual tables cannot run with side
/// effects) and `SQLite`'s defensive mode (no writes to shadow tables / schema corruption).
#[cfg(feature = "sqlite")]
pub(crate) fn open_untrusted_read_only(path: &str) -> Result<rusqlite::Connection, String> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("cannot open database: {e}"))?;
    run_open_hook(&conn);
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

/// One quote- and comment-aware pass over agent-supplied SQL (R4-DB1), following `SQLite`'s
/// tokenizer: `'…'` strings (`''` escapes), `"…"` and `` `…` `` identifiers (doubled-quote
/// escapes), `[…]` identifiers (no escape), `--` to end of line, and `/* … */` (not nested;
/// an unterminated comment or quote runs to the end of input, as in `SQLite`).
#[cfg(feature = "sqlite")]
struct SqlScan {
    /// The SQL with every comment replaced by one space; quoted spans kept verbatim.
    cleaned: String,
    /// `cleaned` with the CONTENT of every quoted span replaced by `_` (the delimiters are
    /// kept), so a search for `;` or `=` only ever sees code.
    masked: String,
}

#[cfg(feature = "sqlite")]
fn scan_sql(sql: &str) -> SqlScan {
    let mut cleaned = String::with_capacity(sql.len());
    let mut masked = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // A named parameter (`$a`, `:a`, `@a`, `#a`). SQLite's tokenizer also accepts a
            // Tcl-style `$a(…)` suffix and reads it as part of the SAME token up to `)` or
            // whitespace, so a quote inside it does not open a string. Treating that quote as
            // one let `SELECT $a(') ; DELETE …'` pass as a single statement (round-4 review).
            '$' | ':' | '@' | '#'
                if chars
                    .peek()
                    .is_some_and(|n| n.is_alphanumeric() || *n == '_' || *n == '$') =>
            {
                cleaned.push(c);
                masked.push(c);
                while let Some(&n) = chars.peek() {
                    if n.is_alphanumeric() || n == '_' || n == '$' || (n == ':' && c == '$') {
                        chars.next();
                        cleaned.push(n);
                        masked.push(n);
                    } else {
                        break;
                    }
                }
                if chars.peek() == Some(&'(') {
                    while let Some(&n) = chars.peek() {
                        if n.is_whitespace() {
                            break;
                        }
                        chars.next();
                        cleaned.push(n);
                        masked.push(if n == '(' || n == ')' { n } else { '_' });
                        if n == ')' {
                            break;
                        }
                    }
                }
            }
            '-' if chars.peek() == Some(&'-') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        break;
                    }
                }
                cleaned.push(' ');
                masked.push(' ');
                // Keep the line break so a line comment still separates tokens.
                cleaned.push('\n');
                masked.push('\n');
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
                cleaned.push(' ');
                masked.push(' ');
            }
            '\'' | '"' | '`' | '[' => {
                let close = if c == '[' { ']' } else { c };
                // `[…]` has no escape; the other three double their closing quote.
                let doubled_escape = c != '[';
                cleaned.push(c);
                masked.push(c);
                while let Some(n) = chars.next() {
                    if n == close {
                        if doubled_escape && chars.peek() == Some(&close) {
                            chars.next();
                            cleaned.push(n);
                            cleaned.push(n);
                            masked.push_str("__");
                            continue;
                        }
                        cleaned.push(n);
                        masked.push(n);
                        break;
                    }
                    cleaned.push(n);
                    masked.push('_');
                }
            }
            _ => {
                cleaned.push(c);
                masked.push(c);
            }
        }
    }
    SqlScan { cleaned, masked }
}

#[cfg(feature = "sqlite")]
fn strip_sql_comments(sql: &str) -> String {
    scan_sql(sql).cleaned
}

/// Number of non-empty statements: the code between top-level `;` separators (a `;` inside a
/// string, quoted identifier or comment is not a separator).
#[cfg(feature = "sqlite")]
fn statement_count(sql: &str) -> usize {
    scan_sql(sql)
        .masked
        .split(';')
        .filter(|s| !s.trim().is_empty())
        .count()
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
    let masked = scan_sql(sql).masked;
    let trimmed = masked.trim_start();
    trimmed.to_lowercase().starts_with("pragma") && trimmed.contains('=')
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

/// `SQLITE_DETERMINISTIC`, as reported in `pragma_function_list.flags`.
#[cfg(feature = "sqlite")]
const SQLITE_DETERMINISTIC_FLAG: i64 = 0x800;

/// Functions `SQLite`'s own compiled-in extensions register as NON-built-in (FTS3/4 and FTS5
/// register their auxiliary functions as overload placeholders, and `MATCH` on any table is
/// authorized as the function `match`; R-Tree and Geopoly register helpers). They are part of
/// `SQLite`, so `query_db` allows them. FTS3's `optimize` (it merges index segments — a write)
/// is deliberately absent.
#[cfg(feature = "sqlite")]
static SQLITE_EXTENSION_FUNCTIONS: &[&str] = &[
    "match",
    "snippet",
    "offsets",
    "matchinfo",
    "bm25",
    "highlight",
    "fts5",
    "fts5_source_id",
    "fts5_get_locale",
    "fts5_locale",
    "fts5_insttoken",
    "rtreenode",
    "rtreedepth",
    "rtreecheck",
    "geopoly_area",
    "geopoly_bbox",
    "geopoly_blob",
    "geopoly_ccw",
    "geopoly_contains_point",
    "geopoly_group_bbox",
    "geopoly_json",
    "geopoly_overlap",
    "geopoly_regular",
    "geopoly_svg",
    "geopoly_within",
    "geopoly_xform",
];

/// `SQLite`'s built-in SQL functions (core scalar / aggregate / window, date and time, JSON,
/// math, percentile), used only when the connection cannot report its functions
/// (`pragma_function_list` unavailable, e.g. `SQLITE_OMIT_INTROSPECTION_PRAGMAS`): then
/// `query_db` allows exactly these plus [`SQLITE_EXTENSION_FUNCTIONS`] and nothing an app
/// registered — fail closed.
#[cfg(feature = "sqlite")]
static SQLITE_BUILTIN_FUNCTIONS: &[&str] = &[
    "->",
    "->>",
    "abs",
    "acos",
    "acosh",
    "asin",
    "asinh",
    "atan",
    "atan2",
    "atanh",
    "avg",
    "ceil",
    "ceiling",
    "changes",
    "char",
    "coalesce",
    "concat",
    "concat_ws",
    "cos",
    "cosh",
    "count",
    "cume_dist",
    "current_date",
    "current_time",
    "current_timestamp",
    "date",
    "datetime",
    "degrees",
    "dense_rank",
    "exp",
    "first_value",
    "floor",
    "format",
    "glob",
    "group_concat",
    "hex",
    "if",
    "ifnull",
    "iif",
    "instr",
    "json",
    "json_array",
    "json_array_insert",
    "json_array_length",
    "json_error_position",
    "json_extract",
    "json_group_array",
    "json_group_object",
    "json_insert",
    "json_object",
    "json_patch",
    "json_pretty",
    "json_quote",
    "json_remove",
    "json_replace",
    "json_set",
    "json_type",
    "json_valid",
    "jsonb",
    "jsonb_array",
    "jsonb_array_insert",
    "jsonb_extract",
    "jsonb_group_array",
    "jsonb_group_object",
    "jsonb_insert",
    "jsonb_object",
    "jsonb_patch",
    "jsonb_remove",
    "jsonb_replace",
    "jsonb_set",
    "julianday",
    "lag",
    "last_insert_rowid",
    "last_value",
    "lead",
    "length",
    "like",
    "likelihood",
    "likely",
    "ln",
    "log",
    "log10",
    "log2",
    "lower",
    "ltrim",
    "max",
    "median",
    "min",
    "mod",
    "nth_value",
    "ntile",
    "nullif",
    "octet_length",
    "percent_rank",
    "percentile",
    "percentile_cont",
    "percentile_disc",
    "pi",
    "pow",
    "power",
    "printf",
    "quote",
    "radians",
    "random",
    "randomblob",
    "rank",
    "replace",
    "round",
    "row_number",
    "rtrim",
    "sign",
    "sin",
    "sinh",
    "soundex",
    "sqlite_compileoption_get",
    "sqlite_compileoption_used",
    "sqlite_log",
    "sqlite_offset",
    "sqlite_source_id",
    "sqlite_version",
    "sqrt",
    "strftime",
    "string_agg",
    "substr",
    "substring",
    "subtype",
    "sum",
    "tan",
    "tanh",
    "time",
    "timediff",
    "total",
    "total_changes",
    "trim",
    "trunc",
    "typeof",
    "unhex",
    "unicode",
    "unistr",
    "unistr_quote",
    "unixepoch",
    "unlikely",
    "upper",
    "zeroblob",
];

/// Never allowed, even though built in: `load_extension` (disabled by default, but it would
/// load arbitrary code).
#[cfg(feature = "sqlite")]
static DENIED_FUNCTIONS: &[&str] = &["load_extension"];

/// What agent-supplied `query_db` SQL may do on one connection, computed from that connection
/// before its authorizer is installed.
#[cfg(feature = "sqlite")]
#[derive(Default)]
struct QueryPolicy {
    /// SQL functions that may run (lowercase names).
    functions: std::collections::HashSet<String>,
    /// Shadow tables of the database's R-Tree / Geopoly virtual tables (lowercase). The R-Tree
    /// module prepares its INSERT/UPDATE/DELETE statements when it connects, so preparing a
    /// write on them must be allowed for an R-Tree table to be readable at all; the `READ_ONLY`
    /// open still stops any write from executing.
    rtree_shadow_tables: std::collections::HashSet<String>,
    /// The first function refused, for a clear error.
    refused_function: std::sync::Mutex<Option<String>>,
}

#[cfg(feature = "sqlite")]
impl QueryPolicy {
    /// The policy for `conn`: `SQLite`'s built-in functions, its own extension functions, and
    /// functions registered DETERMINISTIC may run; any other function (an app can register a
    /// side-effecting one on every connection with `sqlite3_auto_extension`) is refused.
    fn for_connection(conn: &rusqlite::Connection) -> Self {
        let mut functions = Self::listed_functions(conn).unwrap_or_else(|e| {
            tracing::debug!(
                "query_db: pragma_function_list unavailable ({e}); allowing only SQLite's                  built-in functions"
            );
            SQLITE_BUILTIN_FUNCTIONS
                .iter()
                .chain(SQLITE_EXTENSION_FUNCTIONS)
                .map(|f| (*f).to_string())
                .collect()
        });
        for denied in DENIED_FUNCTIONS {
            functions.remove(*denied);
        }
        Self {
            functions,
            rtree_shadow_tables: Self::rtree_shadow_tables(conn),
            refused_function: std::sync::Mutex::new(None),
        }
    }

    /// Functions allowed per the connection's own `pragma_function_list`. A name is allowed only
    /// if EVERY registration under it is built in, deterministic, or one of `SQLite`'s own
    /// extension functions — a non-deterministic app function that overrides a built-in name is
    /// what would actually run, so it taints the name.
    fn listed_functions(
        conn: &rusqlite::Connection,
    ) -> rusqlite::Result<std::collections::HashSet<String>> {
        let mut verdicts: std::collections::HashMap<String, bool> =
            std::collections::HashMap::new();
        let mut stmt = conn.prepare("SELECT name, builtin, flags FROM pragma_function_list")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name = row.get::<_, String>(0)?.to_ascii_lowercase();
            let builtin = row.get::<_, Option<i64>>(1)?.unwrap_or(0) == 1;
            let flags = row.get::<_, Option<i64>>(2)?.unwrap_or(0);
            let ok = builtin
                || flags & SQLITE_DETERMINISTIC_FLAG != 0
                || SQLITE_EXTENSION_FUNCTIONS.contains(&name.as_str());
            let verdict = verdicts.entry(name).or_insert(true);
            *verdict = *verdict && ok;
        }
        Ok(verdicts
            .into_iter()
            .filter_map(|(name, ok)| ok.then_some(name))
            .collect())
    }

    /// Shadow tables of the R-Tree / Geopoly virtual tables in `main`. Best effort: on any
    /// error none are allowed (an R-Tree table is then unreadable, never writable).
    fn rtree_shadow_tables(conn: &rusqlite::Connection) -> std::collections::HashSet<String> {
        let names = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND (substr(sql, 1, 512) LIKE 'CREATE VIRTUAL TABLE % USING rtree%' \
                   OR substr(sql, 1, 512) LIKE 'CREATE VIRTUAL TABLE % USING geopoly%') \
                 LIMIT 1000",
            )
            .and_then(|mut stmt| {
                stmt.query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap_or_default();
        names
            .iter()
            .flat_map(|name| {
                let name = name.to_ascii_lowercase();
                ["_node", "_rowid", "_parent"].map(|suffix| format!("{name}{suffix}"))
            })
            .collect()
    }

    fn allows_function(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        if self.functions.contains(&lower) {
            return true;
        }
        let mut refused = self
            .refused_function
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if refused.is_none() {
            *refused = Some(name.to_string());
        }
        false
    }

    fn is_rtree_shadow(&self, database: Option<&str>, table: &str) -> bool {
        database.is_some_and(|db| db.eq_ignore_ascii_case("main"))
            && self
                .rtree_shadow_tables
                .contains(&table.to_ascii_lowercase())
    }

    /// The function this policy refused, if any (for the error message).
    fn refused_function(&self) -> Option<String> {
        self.refused_function
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// `SQLite` authorizer for agent-supplied `query_db` SQL: default-deny. Reads, recursive CTEs,
/// the functions `policy` allows, and allowlisted read-only PRAGMAs are permitted; everything
/// else (writes, ATTACH, schema changes, transactions, setter PRAGMAs, app-registered
/// non-deterministic functions) is refused before it runs.
#[cfg(feature = "sqlite")]
fn query_authorizer(
    policy: &QueryPolicy,
    ctx: rusqlite::hooks::AuthContext<'_>,
) -> rusqlite::hooks::Authorization {
    use rusqlite::hooks::{AuthAction, Authorization};
    let allow = |ok: bool| {
        if ok {
            Authorization::Allow
        } else {
            Authorization::Deny
        }
    };
    match ctx.action {
        AuthAction::Select | AuthAction::Read { .. } | AuthAction::Recursive => {
            Authorization::Allow
        }
        AuthAction::Function { function_name } => allow(policy.allows_function(function_name)),
        AuthAction::Insert { table_name }
        | AuthAction::Update { table_name, .. }
        | AuthAction::Delete { table_name } => {
            allow(policy.is_rtree_shadow(ctx.database_name, table_name))
        }
        AuthAction::Pragma {
            pragma_name,
            pragma_value,
        } => {
            let name = pragma_name.to_ascii_lowercase();
            allow(
                SAFE_PRAGMAS.contains(&name.as_str())
                    && (pragma_value.is_none() || READ_PRAGMAS_WITH_ARG.contains(&name.as_str())),
            )
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

    // A UX guard, not the security boundary (that is the authorizer + READ_ONLY open): rusqlite's
    // `prepare` silently ignores a trailing statement, so a stacked query would otherwise run
    // only its first statement with no error. Quote-aware, so `SELECT 'a;b'` is one statement.
    if statement_count(sql) > 1 {
        return Err(
            "stacked queries (multiple statements separated by ;) are not allowed".to_string(),
        );
    }
    Ok(())
}

#[cfg(feature = "sqlite")]
// `unit_arg`: on rusqlite 0.32 some setters return `()`; `.into_setup()` on that unit is the
// point (see `SetupOutcome`), so one source compiles fail-closed across the supported range.
#[allow(clippy::unit_arg)]
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
    run_open_hook(&conn);
    // The database is untrusted input (see `open_untrusted_read_only`), and the query is
    // agent-supplied: SQLite's own AUTHORIZER is the enforcement point, not string parsing.
    // It sees every PRAGMA SQLite is about to run — statement form, `PRAGMA name(arg)`, and
    // table-valued `pragma_*()` functions alike (the string checks above missed the last two).
    conn.pragma_update(None, "trusted_schema", false)
        .map_err(|e| format!("failed to harden database connection: {e}"))?;
    conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
        .map_err(|e| format!("failed to harden database connection: {e}"))?;
    // Limit lock waits separately from the CPU deadline enforced below.
    conn.busy_timeout(QUERY_BUSY_TIMEOUT)
        .map_err(|e| format!("failed to set timeout: {e}"))?;

    // Bound both SQLite's per-value/row allocation and CPU time. `busy_timeout`
    // only limits lock waits; it does not stop a CPU-heavy recursive query.
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
        MAX_QUERY_CELL_BYTES,
    )
    .into_setup()
    .map_err(|e| format!("failed to harden database connection: {e}"))?;
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_SQL_LENGTH,
        MAX_QUERY_SQL_BYTES as i32,
    )
    .into_setup()
    .map_err(|e| format!("failed to harden database connection: {e}"))?;
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH,
        MAX_LIKE_PATTERN_BYTES,
    )
    .into_setup()
    .map_err(|e| format!("failed to harden database connection: {e}"))?;
    let started = Instant::now();
    conn.progress_handler(
        QUERY_PROGRESS_OPS,
        Some(move || started.elapsed() >= query_timeout),
    )
    .into_setup()
    .map_err(|e| format!("failed to harden database connection: {e}"))?;
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
    )
    .into_setup()
    .map_err(|e| format!("failed to harden database connection: {e}"))?;

    // The authorizer is installed only now: with the schema loaded (a locked database fails
    // once, at the load above, not again in each policy read) and before the agent's SQL is
    // prepared.
    let policy = Arc::new(QueryPolicy::for_connection(&conn));
    let authorizer_policy = Arc::clone(&policy);
    conn.authorizer(Some(move |ctx: rusqlite::hooks::AuthContext<'_>| {
        query_authorizer(&authorizer_policy, ctx)
    }))
    .into_setup()
    .map_err(|e| format!("failed to harden database connection: {e}"))?;

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| match policy.refused_function() {
            Some(name) => format!(
                "failed to prepare query: the SQL function `{name}` is not allowed — query_db runs \
             only SQLite's built-in functions and functions registered as deterministic (an \
             app-registered function could have side effects)"
            ),
            None => sqlite_query_error("failed to prepare query", e, query_timeout),
        })?;

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

    /// Register a variadic SQL function answering 42 on `conn` (as an app's
    /// `sqlite3_auto_extension` would on every connection), through the C API so the test needs
    /// no rusqlite feature beyond the plugin's own.
    #[allow(unsafe_code)]
    fn register_function(conn: &rusqlite::Connection, name: &str, deterministic: bool) {
        use rusqlite::ffi;
        unsafe extern "C" fn answer(
            ctx: *mut ffi::sqlite3_context,
            _argc: std::os::raw::c_int,
            _argv: *mut *mut ffi::sqlite3_value,
        ) {
            // SAFETY: `ctx` is the live context SQLite passes to a scalar function.
            unsafe { ffi::sqlite3_result_int(ctx, 42) };
        }
        let flags = ffi::SQLITE_UTF8
            | if deterministic {
                ffi::SQLITE_DETERMINISTIC
            } else {
                0
            };
        let name = std::ffi::CString::new(name).unwrap();
        // SAFETY: a valid open connection handle, a NUL-terminated name, and a callback with
        // the signature SQLite expects; no user data or destructor.
        let rc = unsafe {
            ffi::sqlite3_create_function_v2(
                conn.handle(),
                name.as_ptr(),
                -1,
                flags,
                std::ptr::null_mut(),
                Some(answer),
                None,
                None,
                None,
            )
        };
        assert_eq!(rc, ffi::SQLITE_OK);
    }

    /// Run `f` with `hook` applied to every connection this thread opens.
    fn with_open_hook<T>(
        hook: impl Fn(&rusqlite::Connection) + 'static,
        f: impl FnOnce() -> T,
    ) -> T {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                ON_OPEN.with(|h| *h.borrow_mut() = None);
            }
        }
        ON_OPEN.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
        let _reset = Reset;
        f()
    }

    /// R5B-SQLFN1: `query_db` allowed every SQL function, so a side-effecting function the host
    /// registered process-wide (`sqlite3_auto_extension`) ran despite the read-only open. Only
    /// built-in functions and ones registered DETERMINISTIC may run.
    #[test]
    fn app_registered_non_deterministic_functions_are_refused() {
        let (_f, path) = create_test_db();
        with_open_hook(
            |c| {
                register_function(c, "app_side_effect", false);
                register_function(c, "app_pure", true);
                // Overriding a built-in with a non-deterministic function runs the override.
                register_function(c, "lower", false);
            },
            || {
                let err = query(&path, "SELECT app_side_effect()", &[], None).unwrap_err();
                assert!(err.contains("app_side_effect"), "{err}");
                let err = query(&path, "SELECT lower('A')", &[], None).unwrap_err();
                assert!(err.contains("lower"), "{err}");
                // Hidden in a CTE it is still refused.
                let err = query(
                    &path,
                    "WITH x AS (SELECT app_side_effect() AS v) SELECT v FROM x",
                    &[],
                    None,
                )
                .unwrap_err();
                assert!(err.contains("app_side_effect"), "{err}");
                let r = query(&path, "SELECT app_pure() AS v", &[], None).unwrap();
                assert_eq!(r["rows"][0]["v"], 42);
                assert!(query(&path, "SELECT upper('a'), abs(-1)", &[], None).is_ok());
            },
        );
    }

    /// The function policy must not break `SQLite`'s own functions: core scalar / aggregate /
    /// window, date/time, JSON, and the FTS3/4, FTS5 and R-Tree extension functions (which
    /// `SQLite` registers as non-built-in overloads).
    #[test]
    fn builtin_and_extension_functions_still_work() {
        let (_f, path) = create_test_db();
        let conn = rusqlite::Connection::open(&path).unwrap();
        let fts = conn
            .execute_batch(
                "CREATE VIRTUAL TABLE f5 USING fts5(body); INSERT INTO f5 VALUES ('hello world');
                 CREATE VIRTUAL TABLE f4 USING fts4(body); INSERT INTO f4 VALUES ('hello there');",
            )
            .is_ok();
        let rtree = conn
            .execute_batch(
                "CREATE VIRTUAL TABLE r USING rtree(id, x0, x1); INSERT INTO r VALUES (1, 0, 5);",
            )
            .is_ok();
        drop(conn);
        let mut sqls = vec![
            "SELECT abs(-1), lower('A'), upper('a'), length('x'), substr('abc', 2), \
             coalesce(NULL, 1), printf('%d', 5), hex('a'), typeof(1), round(1.5), \
             replace('a', 'a', 'b'), instr('ab', 'b'), trim(' a '), random() IS NOT NULL",
            "SELECT count(*), sum(score), avg(score), max(score), min(score), \
             group_concat(name) FROM users",
            "SELECT name, row_number() OVER (ORDER BY score), rank() OVER (ORDER BY score), \
             lag(name) OVER (ORDER BY id) FROM users",
            "SELECT date('now'), datetime('now'), julianday('now'), strftime('%Y', 'now'), \
             unixepoch('now')",
            "SELECT json_extract('{\"a\":1}', '$.a'), json_object('a', 1), json_array(1, 2), \
             '{\"a\":1}' -> '$.a', '{\"a\":1}' ->> '$.a'",
            "SELECT * FROM json_each('[1,2]')",
        ];
        if fts {
            sqls.push(
                "SELECT bm25(f5), highlight(f5, 0, '[', ']'), snippet(f5, 0, '[', ']', '..', 5) \
                 FROM f5 WHERE f5 MATCH 'hello'",
            );
            sqls.push(
                "SELECT snippet(f4), offsets(f4), matchinfo(f4) FROM f4 WHERE f4 MATCH 'hello'",
            );
        }
        if rtree {
            sqls.push("SELECT id FROM r WHERE x0 >= 0 AND x1 <= 10");
        }
        for sql in sqls {
            let r = query(&path, sql, &[], None);
            assert!(r.is_ok(), "{sql}: {r:?}");
        }
        // Still refused: loading an extension.
        assert!(query(&path, "SELECT load_extension('x')", &[], None).is_err());
    }

    /// The fail-closed fallback list (used when a connection cannot report its functions) must
    /// cover every function this `SQLite` reports as built in, and every non-built-in function
    /// `SQLite`'s own extensions register must be known — so a newer `SQLite` that adds one fails
    /// this test instead of silently refusing it in `query_db`.
    #[test]
    fn fallback_function_lists_cover_this_sqlite() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let mut stmt = conn
            .prepare("SELECT DISTINCT name, builtin FROM pragma_function_list")
            .unwrap();
        let rows: Vec<(String, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(rows.len() > 50, "{rows:?}");
        let known_non_builtin = ["fts3_tokenizer", "optimize"];
        for (name, builtin) in rows {
            let known = if DENIED_FUNCTIONS.contains(&name.as_str()) {
                true
            } else if builtin == 1 {
                SQLITE_BUILTIN_FUNCTIONS.contains(&name.as_str())
            } else {
                SQLITE_EXTENSION_FUNCTIONS.contains(&name.as_str())
                    || known_non_builtin.contains(&name.as_str())
            };
            assert!(known, "function `{name}` (builtin={builtin}) is not listed");
        }
    }

    /// Register a virtual-table module named `name` on `conn` (as a host's
    /// `sqlite3_auto_extension` would) whose constructor only counts how often it ran and fails.
    #[allow(unsafe_code)]
    fn register_counting_module(
        conn: &rusqlite::Connection,
        name: &str,
        connects: &'static std::sync::atomic::AtomicUsize,
    ) {
        use rusqlite::ffi;
        use std::os::raw::{c_char, c_int, c_void};
        unsafe extern "C" fn construct(
            _db: *mut ffi::sqlite3,
            aux: *mut c_void,
            _argc: c_int,
            _argv: *const *const c_char,
            _vtab: *mut *mut ffi::sqlite3_vtab,
            _err: *mut *mut c_char,
        ) -> c_int {
            // SAFETY: `aux` is the `&'static AtomicUsize` passed as client data below.
            let counter = unsafe { &*aux.cast::<std::sync::atomic::AtomicUsize>() };
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ffi::SQLITE_ERROR
        }
        // SAFETY: an all-zero `sqlite3_module` is valid (version 0, every method absent).
        let mut module: ffi::sqlite3_module = unsafe { std::mem::zeroed() };
        module.xCreate = Some(construct);
        module.xConnect = Some(construct);
        let module: &'static ffi::sqlite3_module = Box::leak(Box::new(module));
        let name = std::ffi::CString::new(name).unwrap();
        // SAFETY: a valid connection handle, a NUL-terminated name, a module that outlives the
        // connection (leaked) and client data that is `'static`; no destructor.
        let rc = unsafe {
            ffi::sqlite3_create_module_v2(
                conn.handle(),
                name.as_ptr(),
                module,
                std::ptr::from_ref(connects).cast_mut().cast(),
                None,
            )
        };
        assert_eq!(rc, ffi::SQLITE_OK);
    }

    /// Plant `CREATE VIRTUAL TABLE <table> USING <module>(x)` in the schema without the module
    /// being present (`writable_schema`), as a crafted database file can.
    fn plant_virtual_table(path: &Path, table: &str, module: &str) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA writable_schema=ON;").unwrap();
        conn.execute(
            "INSERT INTO sqlite_master (type, name, tbl_name, rootpage, sql) \
             VALUES ('table', ?1, ?1, 0, ?2)",
            [
                table.to_string(),
                format!("CREATE VIRTUAL TABLE {table} USING {module}(x)"),
            ],
        )
        .unwrap();
    }

    /// R5B-QC1: on `SQLite` >= 3.44 a whole-database `quick_check` connects every virtual
    /// table whose module is registered and runs its integrity routine — module code, including
    /// a module the HOST registered process-wide. With such a module registered and a virtual
    /// table using it in the file, `db_health` must check the ordinary tables one by one instead,
    /// so no module code runs.
    #[test]
    fn db_health_never_runs_a_host_registered_module() {
        static CONNECTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let (_f, path) = create_test_db();
        plant_virtual_table(&path, "planted", "victauri_marker");
        let long = Duration::from_secs(10);
        let r = with_open_hook(
            |c| register_counting_module(c, "victauri_marker", &CONNECTS),
            || db_health_report(path.to_str().unwrap(), long, long).unwrap(),
        );
        assert_eq!(
            CONNECTS.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the module's constructor ran: {r}"
        );
        assert_eq!(r["integrity_check"], "ok", "{r}");
        assert_eq!(r["integrity_check_kind"], "quick_check (per table)", "{r}");
        let note = r["integrity_check_note"].as_str().unwrap_or_default();
        assert!(note.contains("victauri_marker"), "{r}");
        let planted = r["tables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "planted")
            .expect("the planted table is listed");
        assert!(planted["row_count"].is_null(), "{planted}");
    }

    /// Per-table checking still finds corruption in an ordinary table.
    #[test]
    fn db_health_per_table_check_still_reports_problems() {
        static CONNECTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let file = tempfile::NamedTempFile::with_suffix(".sqlite").unwrap();
        let path = file.path().to_path_buf();
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT);
                 INSERT INTO t VALUES (1, 'a'), (2, NULL), (3, 'c');",
            )
            .unwrap();
            // Declare a NOT NULL the stored rows violate (quick_check verifies NOT NULL).
            conn.execute_batch(
                "PRAGMA writable_schema=ON;
                 UPDATE sqlite_master SET sql = 'CREATE TABLE t (id INTEGER PRIMARY KEY,                  v TEXT NOT NULL)' WHERE name = 't';",
            )
            .unwrap();
        }
        plant_virtual_table(&path, "planted", "victauri_marker2");
        let long = Duration::from_secs(10);
        let r = with_open_hook(
            |c| register_counting_module(c, "victauri_marker2", &CONNECTS),
            || db_health_report(path.to_str().unwrap(), long, long).unwrap(),
        );
        assert_eq!(CONNECTS.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(r["integrity_check_kind"], "quick_check (per table)", "{r}");
        let integrity = r["integrity_check"].as_str().unwrap();
        assert!(integrity.contains("NULL value in t.v"), "{r}");
    }

    /// Without a host-registered module the whole database is still checked, `SQLite`'s own
    /// virtual tables included (an FTS5 table), and a virtual table whose module is not loaded
    /// is simply not connected.
    #[test]
    fn db_health_checks_the_whole_database_when_only_builtin_modules_exist() {
        let (_f, path) = create_test_db();
        let conn = rusqlite::Connection::open(&path).unwrap();
        if conn
            .execute_batch(
                "CREATE VIRTUAL TABLE docs USING fts5(body); INSERT INTO docs VALUES ('x');",
            )
            .is_err()
        {
            return; // this SQLite build has no FTS5
        }
        drop(conn);
        plant_virtual_table(&path, "unknown_vtab", "not_a_loaded_module");
        let long = Duration::from_secs(10);
        let r = db_health_report(path.to_str().unwrap(), long, long).unwrap();
        assert_eq!(r["integrity_check"], "ok", "{r}");
        assert_eq!(r["integrity_check_kind"], "quick_check", "{r}");
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

    /// R5-DB1: virtual-table detection was `sql LIKE 'CREATE VIRTUAL%'`, which a crafted file
    /// defeats with any other spelling `SQLite` still parses as a virtual table (planted through
    /// `writable_schema`) — and then `count(*)` ran the module's code.
    #[test]
    fn db_health_does_not_count_a_disguised_virtual_table() {
        for (i, disguise) in [
            "CREATE  VIRTUAL TABLE",
            "CREATE\tVIRTUAL TABLE",
            "CREATE\nVIRTUAL TABLE",
            "CREATE/**/VIRTUAL TABLE",
            "CREATE -- c\nVIRTUAL TABLE",
            "create virtual table",
        ]
        .into_iter()
        .enumerate()
        {
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
            conn.execute_batch("PRAGMA writable_schema=ON;").unwrap();
            let changed = conn
                .execute(
                    "UPDATE sqlite_master SET sql = replace(sql, 'CREATE VIRTUAL TABLE', ?1) \
                     WHERE name = 'docs'",
                    [disguise],
                )
                .unwrap();
            assert_eq!(changed, 1);
            drop(conn);
            // SQLite itself still reads it as the virtual table.
            let check = rusqlite::Connection::open(file.path()).unwrap();
            let sql: String = check
                .query_row("SELECT sql FROM sqlite_master WHERE name='docs'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert!(sql.starts_with(disguise), "case {i}: {sql}");
            check
                .execute_batch("INSERT INTO docs(body) VALUES ('still fts5')")
                .unwrap_or_else(|e| panic!("case {i}: not a working virtual table: {e}"));
            drop(check);

            let long = Duration::from_secs(10);
            let r = db_health_report(file.path().to_str().unwrap(), long, long).unwrap();
            let tables = r["tables"].as_array().unwrap();
            let docs = tables.iter().find(|t| t["name"] == "docs").unwrap();
            assert!(
                docs["row_count"].is_null(),
                "case {i} ({disguise:?}): a virtual table was counted: {docs}"
            );
            assert_eq!(docs["virtual"], true, "case {i}: {docs}");
            let plain = tables.iter().find(|t| t["name"] == "plain").unwrap();
            assert_eq!(plain["row_count"], 1, "case {i}: {plain}");
        }
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

    /// R4-DEP1: the setters that install the authorizer, deadlines and size limits return `()`,
    /// `i32` or `rusqlite::Result` depending on the rusqlite version in the supported range. A
    /// failed install must surface as an error on every version, never be dropped.
    #[test]
    fn a_failed_security_setup_is_an_error_on_every_rusqlite_shape() {
        assert_eq!(().into_setup(), Ok(()));
        assert_eq!(0_i32.into_setup(), Ok(()));
        assert_eq!(Ok::<i32, rusqlite::Error>(7).into_setup(), Ok(()));
        assert!(
            Err::<(), _>(rusqlite::Error::InvalidQuery)
                .into_setup()
                .is_err()
        );
    }

    /// R4-DB1: the pre-checks were quote-unaware — a `;`, `--` or `/*` inside a string literal
    /// or quoted identifier was treated as a statement separator or comment. Reproduced live:
    /// `SELECT 'a;b'` and a `LIKE '%;%'` filter were refused as "stacked queries".
    #[test]
    fn quoted_separators_and_comment_markers_are_not_code() {
        for sql in [
            "SELECT 'a;b'",
            "SELECT name FROM t WHERE msg LIKE '%;%'",
            "SELECT 'x--y'",
            "SELECT '/* not a comment */;'",
            "SELECT \"a;b\" FROM t",
            "SELECT [a;b] FROM t",
            "SELECT `a;b` FROM t",
            "SELECT ''';'",
            "SELECT \"x\"\";\" FROM t",
            "SELECT 'héllo;wörld — ✓'",
            "SELECT 'a;b';",
            "SELECT 1 -- trailing ; comment",
            "SELECT 1 /* ; */",
            "PRAGMA table_info('a;b')",
            "PRAGMA table_info([a=b])",
        ] {
            assert_eq!(validate_query(sql), Ok(()), "must allow: {sql}");
        }
    }

    /// Round-4 review: `SQLite` reads a Tcl-style parameter `$a(...)` (also `:a(`, `@a(`, `#a(`)
    /// as ONE token up to `)` or whitespace, so a quote inside it is not a string — the quote-
    /// aware scanner treated it as one and let a real second statement through. Verified
    /// against `SQLite`: `SELECT $a(') ; DELETE FROM users --'` is two statements.
    #[test]
    fn tcl_style_parameters_do_not_hide_a_stacked_statement() {
        for sql in [
            "SELECT $a(') ; DELETE FROM users --'",
            "SELECT :a(') ; DELETE FROM users --'",
            "SELECT @a(') ; DELETE FROM users --'",
            "SELECT #a(') ; DELETE FROM users --'",
        ] {
            let err = validate_query(sql).expect_err(sql);
            assert!(err.contains("stacked queries"), "{sql}: {err}");
        }
        for sql in [
            "SELECT * FROM t WHERE id = :id",
            "SELECT * FROM t WHERE a = $a AND b = @b",
            "SELECT $a(x) FROM t",
            "SELECT '$a(' || x FROM t",
            "SELECT '#1;2' AS s",
        ] {
            assert_eq!(validate_query(sql), Ok(()), "must allow: {sql}");
        }
    }

    /// R4-DB1: the other direction — a comment marker inside a string made the old stripper
    /// swallow a REAL second statement, so it passed the stacked-query check.
    #[test]
    fn real_stacked_statements_are_refused_even_after_quoted_markers() {
        for sql in [
            "SELECT 1; SELECT 2",
            "SELECT 1; ATTACH 'x.db' AS y",
            "SELECT 1 /* ; */ ; DELETE FROM users",
            "SELECT 1; -- hidden\nDELETE FROM users",
            "SELECT '--'; DELETE FROM users",
            "SELECT '/*'; SELECT '*/'",
            "SELECT ''';'; DROP TABLE users",
            "SELECT \"--\"; DELETE FROM users",
            "SELECT [/*]; DELETE FROM users",
            "SELECT 'é'; DELETE FROM users",
        ] {
            let err = validate_query(sql).expect_err(sql);
            assert!(err.contains("stacked queries"), "{sql}: {err}");
        }
        // Comment-hidden writes are still refused as writes.
        for sql in [
            "/* SELECT */ DELETE FROM users",
            "-- SELECT\nDELETE FROM users",
            "/*/ SELECT */ DELETE FROM users",
        ] {
            let err = validate_query(sql).expect_err(sql);
            assert!(err.contains("read-only"), "{sql}: {err}");
        }
        // A write whose `=` hides after a quoted `--` is still a PRAGMA write.
        let err = validate_query("PRAGMA user_version = '--'").unwrap_err();
        assert!(err.contains("PRAGMA writes"), "{err}");
    }

    /// R4-DB1: the old stripper pushed `bytes[i] as char`, turning every non-ASCII byte into a
    /// separate Latin-1 char in the validation copy.
    #[test]
    fn sql_scan_keeps_non_ascii_and_masks_only_quoted_content() {
        let s = scan_sql("SELECT 'é;✓' /* ; */, \"ü\"\"x\" -- ü;\nFROM [t;1]");
        assert_eq!(s.cleaned, "SELECT 'é;✓'  , \"ü\"\"x\"  \nFROM [t;1]");
        assert_eq!(s.masked, "SELECT '___'  , \"____\"  \nFROM [___]");
        assert_eq!(strip_sql_comments("SELECT 'naïve'"), "SELECT 'naïve'");
        assert_eq!(statement_count("SELECT 1;"), 1);
        assert_eq!(statement_count("SELECT 1;;  ;"), 1);
        assert_eq!(statement_count("SELECT 1; SELECT 2"), 2);
        assert_eq!(statement_count("SELECT 'unterminated; DROP"), 1);
    }

    /// R4-DB1 end to end: the live-failing queries run and return the literal intact.
    #[test]
    fn quoted_semicolons_run_against_a_real_database() {
        let (_f, path) = create_test_db();
        let r = query(
            &path,
            "SELECT 'a;b' AS v, 'x--y' AS w, 'é;✓' AS u",
            &[],
            None,
        )
        .unwrap();
        assert_eq!(r["rows"][0]["v"], "a;b");
        assert_eq!(r["rows"][0]["w"], "x--y");
        assert_eq!(r["rows"][0]["u"], "é;✓");
        let r = query(
            &path,
            "SELECT name FROM users WHERE name LIKE '%;%'",
            &[],
            None,
        )
        .unwrap();
        assert_eq!(r["row_count"], 0);
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
