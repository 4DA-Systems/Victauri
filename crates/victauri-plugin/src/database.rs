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
    /// How many times the watchdog woke up (test builds only): pins that it parks until the
    /// deadline instead of polling in fixed steps.
    #[cfg(test)]
    wakes: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(feature = "sqlite")]
impl InterruptGuard {
    pub(crate) fn arm(conn: &rusqlite::Connection, deadline: Duration) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let interrupt = conn.get_interrupt_handle();
        let done_for_thread = done.clone();
        #[cfg(test)]
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        #[cfg(test)]
        let wakes_for_thread = Arc::clone(&wakes);
        // The watchdog PARKS until the deadline and is unparked on drop, so disarming is
        // immediate. (It used to sleep in fixed 25ms steps that the drop then joined — ~25ms
        // per guard however fast the guarded query was, which capped db_health at ~200 tables
        // per 5s budget even on a tiny database.)
        let handle = std::thread::spawn(move || {
            let start = Instant::now();
            loop {
                #[cfg(test)]
                wakes_for_thread.fetch_add(1, Ordering::Relaxed);
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
            #[cfg(test)]
            wakes,
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

/// Victauri accepts a RANGE of rusqlite versions (`>=0.32, <0.42`) so it unifies with the app's
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
    // `&mut`: `progress_handler` takes `&mut self` from rusqlite 0.41 (`&self` before).
    conn: &mut rusqlite::Connection,
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
        f(&*conn)
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
    on_db_stack(|| {
        check_schema_nesting(Path::new(path))
            .map_err(|e| format!("cannot read database metadata: {e}"))?;
        db_health_report_budgeted(path, DB_HEALTH_META_BUDGET, count_budget, check_budget)
    })
}

#[cfg(feature = "sqlite")]
fn db_health_report_budgeted(
    path: &str,
    meta_budget: Duration,
    count_budget: Duration,
    check_budget: Duration,
) -> Result<serde_json::Value, String> {
    let mut conn = open_untrusted_read_only(path)?;
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
    let meta = run_bounded(&mut conn, meta_budget, |c| {
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
                meta_budget.as_millis()
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
                match run_bounded(&mut conn, remaining, |c| {
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
            let integrity = match run_bounded(&mut conn, check_budget, |c| {
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
            let integrity = match run_bounded(&mut conn, check_budget, |c| {
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
type OpenHook = Arc<dyn Fn(&rusqlite::Connection) + Send + Sync>;

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

/// Stack for database work. `SQLite` recurses over nested SELECTs (parsing, copying and freeing
/// them, and expanding views) with no limit of its own, and a stack overflow is not a panic: it
/// ends the host process. Reserved, not committed: untouched pages cost address space only.
#[cfg(feature = "sqlite")]
const DB_STACK_BYTES: usize = 64 * 1024 * 1024;
/// Deepest parenthesis nesting accepted in agent SQL or in one schema entry. Measured on the
/// dev profile: well under 1 KiB of stack per level.
#[cfg(feature = "sqlite")]
const MAX_SQL_NESTING: usize = 256;
/// Most SELECTs accepted in agent SQL or in one schema entry (a compound is capped at 500 by
/// `SQLite` itself). CTEs chain with no parentheses to count, and preparing a chain is quadratic in
/// a call no deadline can interrupt: measured 209 s for 30,974 CTEs (the 1 MB size cap).
#[cfg(feature = "sqlite")]
const MAX_SQL_SELECTS: usize = 2_000;
/// Most nesting one chain of views may add up to, a chain being views that select from each other
/// (see [`deepest_view_chain`]). Each view on it costs 1 plus its own nesting or SELECT count,
/// whichever is larger. Preparing a chain is quadratic too: measured 195 s for 9,900 views, so about
/// 8 s here. Views that repeat one another (`FROM v a, v b`, doubling per level) are not charged
/// for the repeats: `SQLite` refuses those itself, when one is used, within seconds (measured: at
/// most 9 s, "too many FROM clause terms" or "too many references").
#[cfg(feature = "sqlite")]
const MAX_SCHEMA_VIEW_NESTING: usize = 2_000;
/// Most view text [`check_schema_nesting`] holds to work out the chains.
#[cfg(feature = "sqlite")]
const MAX_SCHEMA_VIEW_TEXT_BYTES: usize = 16 * 1024 * 1024;
/// The longest schema entry the nesting check itself will parse (see [`check_schema_nesting`]).
#[cfg(feature = "sqlite")]
const SCHEMA_CHECK_PARSE_BYTES: i32 = 4_096;
/// Budget for [`check_schema_nesting`] once it has the file.
#[cfg(feature = "sqlite")]
const SCHEMA_CHECK_BUDGET: Duration = Duration::from_secs(3);

/// Run `f` on a thread with [`DB_STACK_BYTES`] of stack and wait for it (a panic carries over).
#[cfg(feature = "sqlite")]
fn on_db_stack<T: Send>(f: impl FnOnce() -> Result<T, String> + Send) -> Result<T, String> {
    #[cfg(test)]
    let hook = ON_OPEN.with(|h| h.borrow().clone());
    std::thread::scope(|scope| {
        let thread = std::thread::Builder::new()
            .name("victauri-db".to_string())
            .stack_size(DB_STACK_BYTES)
            .spawn_scoped(scope, move || {
                #[cfg(test)]
                ON_OPEN.with(|h| *h.borrow_mut() = hook);
                f()
            })
            .map_err(|e| format!("cannot start the database thread: {e}"))?;
        thread
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// Deepest parenthesis nesting in `sql` and its number of SELECTs, ignoring strings, identifiers
/// and comments.
#[cfg(feature = "sqlite")]
fn sql_nesting(sql: &str) -> (usize, usize) {
    let masked = scan_sql(sql).masked.to_ascii_lowercase();
    let (mut depth, mut deepest) = (0usize, 0usize);
    for b in masked.bytes() {
        if b == b'(' {
            depth += 1;
            deepest = deepest.max(depth);
        } else if b == b')' {
            depth = depth.saturating_sub(1);
        }
    }
    let selects = masked
        .split(|c: char| !is_sqlite_id_char(c))
        .filter(|word| *word == "select")
        .count();
    (deepest, selects)
}

/// One token of a view's definition (see [`sql_words`]).
#[cfg(feature = "sqlite")]
struct SqlWord {
    /// Lowercase, as names compare without regard to ASCII case.
    text: String,
    /// Written in quotes: then it is a name, never a keyword or punctuation.
    quoted: bool,
}

/// A view's definition split into tokens: bare words, the content of quoted spans (`SQLite` takes a
/// string where a name is expected, so all four quote styles can name a view), and `.` `,` `(` `)`.
/// A parameter is not read as `SQLite` reads it (`:select` gives the word `select`): a view holding
/// one does not load at all.
#[cfg(feature = "sqlite")]
fn sql_words(sql: &str) -> Vec<SqlWord> {
    let cleaned = scan_sql(sql).cleaned;
    let bare = |text: String| SqlWord {
        text,
        quoted: false,
    };
    let mut words = Vec::new();
    let mut chars = cleaned.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' | '`' | '[' => {
                let close = if c == '[' { ']' } else { c };
                let mut word = String::new();
                while let Some(n) = chars.next() {
                    if n != close {
                        word.push(n);
                    } else if c != '[' && chars.peek() == Some(&close) {
                        chars.next();
                        word.push(n);
                    } else {
                        break;
                    }
                }
                words.push(SqlWord {
                    text: word.to_ascii_lowercase(),
                    quoted: true,
                });
            }
            '.' | ',' | '(' | ')' => words.push(bare(c.to_string())),
            c if is_sqlite_id_char(c) => {
                let mut word = String::from(c);
                while let Some(n) = chars.next_if(|n| is_sqlite_id_char(*n)) {
                    word.push(n);
                }
                words.push(bare(word.to_ascii_lowercase()));
            }
            _ => {}
        }
    }
    words
}

/// The name a schema entry defines a view under, and how many of `words` that takes, if it is a
/// view. `SQLite` builds its schema by parsing this text, and at most compares the `type` and
/// `name` columns with it ignoring case (not at all under `writable_schema`), so a working view
/// can say `type='VIEW'` or a BLOB.
#[cfg(feature = "sqlite")]
fn view_definition(words: &[SqlWord]) -> Option<(&str, usize)> {
    let word = |i: usize| words.get(i).map(|w| w.text.as_str());
    let mut at = 1;
    if word(0)? != "create" {
        return None;
    }
    if matches!(word(at), Some("temp" | "temporary")) {
        at += 1;
    }
    if word(at)? != "view" {
        return None;
    }
    at += 1;
    if (word(at), word(at + 1), word(at + 2)) == (Some("if"), Some("not"), Some("exists")) {
        at += 3;
    }
    if word(at + 1) == Some(".") {
        at += 2;
    }
    Some((word(at)?, at + 1))
}

/// What a view's body may select from: every name where `SQLite`'s grammar takes a table. That is a
/// FROM clause (to its WHERE, GROUP, HAVING, ORDER or LIMIT, and not a join's condition, which a
/// `,` or JOIN ends) and the name after IN (`x IN v`, which expands `v` from inside any expression).
/// A result list and the other clauses name only columns, and subqueries in them are read by the
/// same rules. Those keywords are all reserved, so bare they cannot be names, and anything else
/// spelled like a name in a FROM clause (an alias, a table-valued function's argument) counts,
/// which can only overcharge. Strings count: `SQLite` takes `FROM 'v1'` as a name.
#[cfg(feature = "sqlite")]
fn table_mentions(body: &[SqlWord]) -> Vec<&str> {
    #[derive(Clone, Copy, PartialEq)]
    enum Clause {
        Expressions,
        Tables,
        JoinCondition,
    }
    let name_at = |at: usize| {
        body.get(at)
            .filter(|w| w.quoted || !matches!(w.text.as_str(), "(" | ")" | "," | "."))
    };
    let mut mentions = Vec::new();
    // One clause per open parenthesis. A parenthesis in a FROM clause is more of it (`FROM (a, b)`);
    // one anywhere else holds expressions until a subquery says otherwise.
    let mut clauses = vec![Clause::Expressions];
    for (at, word) in body.iter().enumerate() {
        let Some(clause) = clauses.last_mut() else {
            break;
        };
        if word.quoted {
            if *clause == Clause::Tables {
                mentions.push(word.text.as_str());
            }
            continue;
        }
        match word.text.as_str() {
            "(" => {
                let inner = if *clause == Clause::Tables {
                    Clause::Tables
                } else {
                    Clause::Expressions
                };
                clauses.push(inner);
            }
            ")" => {
                if clauses.len() > 1 {
                    clauses.pop();
                }
            }
            "." => {}
            "," => {
                if *clause == Clause::JoinCondition {
                    *clause = Clause::Tables;
                }
            }
            "select" | "where" | "group" | "having" | "order" | "limit" => {
                *clause = Clause::Expressions;
            }
            "from" | "join" => *clause = Clause::Tables,
            "on" | "using" if *clause == Clause::Tables => *clause = Clause::JoinCondition,
            "in" => {
                // `x IN v` or `x IN main.v`; `x IN (…)` is read as what it holds.
                if let Some(name) = name_at(at + 1) {
                    mentions.push(name.text.as_str());
                    if body.get(at + 2).is_some_and(|w| !w.quoted && w.text == ".")
                        && let Some(table) = name_at(at + 3)
                    {
                        mentions.push(table.text.as_str());
                    }
                }
            }
            name => {
                if *clause == Clause::Tables {
                    mentions.push(name);
                }
            }
        }
    }
    mentions
}

/// The most nesting any chain of views adds up to: the largest sum of `cost` along views that
/// select from each other, as [`table_mentions`] reads it. Mentions can close a cycle a real chain
/// runs through (an alias spelled like a view), so views that reach each other are charged
/// together, each once. That bounds any real chain among them.
#[cfg(feature = "sqlite")]
fn deepest_view_chain(views: &[(String, usize)]) -> usize {
    const UNSEEN: usize = 0;
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut own: Vec<usize> = Vec::new();
    let mut parsed = Vec::with_capacity(views.len());
    for (text, cost) in views {
        let words = sql_words(text);
        let Some((name, body)) = view_definition(&words) else {
            continue;
        };
        // A repeated name is one node charged for all of its definitions.
        let node = *index.entry(name.to_string()).or_insert_with(|| {
            own.push(0);
            own.len() - 1
        });
        own[node] = own[node].max(*cost);
        parsed.push((node, words, body));
    }
    let mut deps: Vec<Vec<usize>> = vec![Vec::new(); own.len()];
    for (node, words, body) in &parsed {
        let found = table_mentions(&words[*body..])
            .into_iter()
            .filter_map(|name| index.get(name).copied());
        deps[*node].extend(found);
    }
    drop(parsed);
    // Tarjan's strongly connected components, depth first with an explicit stack: a chain can be as
    // long as the file allows. A group closes only after every group it depends on, so `chain` is
    // final for all of those; for its own members it is still 0.
    let mut order = vec![UNSEEN; own.len()];
    let mut low = vec![UNSEEN; own.len()];
    let mut open = vec![false; own.len()];
    let mut group: Vec<usize> = Vec::new();
    let mut chain = vec![0usize; own.len()];
    let mut visited = 0;
    let mut deepest = 0;
    for root in 0..own.len() {
        if order[root] != UNSEEN {
            continue;
        }
        visited += 1;
        order[root] = visited;
        low[root] = visited;
        open[root] = true;
        group.push(root);
        let mut stack = vec![(root, 0usize)];
        while let Some((node, next)) = stack.last_mut() {
            let node = *node;
            if let Some(&dep) = deps[node].get(*next) {
                *next += 1;
                if order[dep] == UNSEEN {
                    visited += 1;
                    order[dep] = visited;
                    low[dep] = visited;
                    open[dep] = true;
                    group.push(dep);
                    stack.push((dep, 0));
                } else if open[dep] {
                    low[node] = low[node].min(order[dep]);
                }
                continue;
            }
            stack.pop();
            if let Some((parent, _)) = stack.last() {
                low[*parent] = low[*parent].min(low[node]);
            }
            if low[node] != order[node] {
                continue;
            }
            let start = group.iter().rposition(|m| *m == node).unwrap_or(0);
            let members = group.split_off(start);
            let cost = members
                .iter()
                .fold(0usize, |sum, m| sum.saturating_add(own[*m]));
            let below = members
                .iter()
                .flat_map(|m| &deps[*m])
                .map(|d| chain[*d])
                .max()
                .unwrap_or(0);
            let total = cost.saturating_add(below);
            for m in members {
                open[m] = false;
                chain[m] = total;
            }
            deepest = deepest.max(total);
        }
    }
    deepest
}

/// Refuse a database whose schema is nested deeply enough to threaten the stack. `SQLite` parses
/// the whole schema on the first statement, so a crafted file can overflow the stack with no help
/// from the query, and a larger stack alone cannot cover a schema entry of up to 1 GB.
///
/// So the text is read first, on a connection that does not parse it: with `writable_schema`,
/// `SQLite` tolerates a schema entry it cannot parse (the first statement fails, the next one
/// runs), and the SQL length limit makes every long entry unparseable. Entries within the limit
/// are too short to nest dangerously.
///
/// The file can change between this check and the real open; the stack is the margin for that.
#[cfg(feature = "sqlite")]
// `unit_arg`: see `SetupOutcome`.
#[allow(clippy::unit_arg)]
fn check_schema_nesting(path: &Path) -> Result<(), String> {
    const READ_SCHEMA: &str = "SELECT type, name, sql FROM sqlite_master";
    let harden = |e: String| format!("failed to harden database connection: {e}");
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("failed to open database: {e}"))?;
    conn.pragma_update(None, "trusted_schema", false)
        .map_err(|e| harden(e.to_string()))?;
    conn.pragma_update(None, "writable_schema", true)
        .map_err(|e| harden(e.to_string()))?;
    conn.busy_timeout(QUERY_BUSY_TIMEOUT)
        .map_err(|e| harden(e.to_string()))?;
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_SQL_LENGTH,
        SCHEMA_CHECK_PARSE_BYTES,
    )
    .into_setup()
    .map_err(harden)?;
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
        MAX_QUERY_SQL_BYTES as i32,
    )
    .into_setup()
    .map_err(harden)?;
    let _interrupt = InterruptGuard::arm(&conn, SCHEMA_CHECK_BUDGET);
    let load_error = |e: rusqlite::Error| {
        sqlite_query_error("failed to load database schema", e, SCHEMA_CHECK_BUDGET)
    };
    // The retry is for a schema entry that cannot be parsed. A lock has had its wait already, and
    // waiting again would break the one short lock wait a query promises.
    let mut stmt = conn
        .prepare(READ_SCHEMA)
        .or_else(|e| match e.sqlite_error_code() {
            Some(
                rusqlite::ffi::ErrorCode::DatabaseBusy | rusqlite::ffi::ErrorCode::DatabaseLocked,
            ) => Err(e),
            _ => conn.prepare(READ_SCHEMA),
        })
        .map_err(load_error)?;
    let mut rows = stmt.query([]).map_err(load_error)?;
    let mut views: Vec<(String, usize)> = Vec::new();
    let mut view_bytes = 0usize;
    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(e) if e.sqlite_error_code() == Some(rusqlite::ffi::ErrorCode::TooBig) => {
                return Err(format!(
                    "refusing to open this database: a schema entry is too long (over \
                     {MAX_QUERY_SQL_BYTES} bytes), which SQLite would have to parse in full"
                ));
            }
            Err(e) => return Err(load_error(e)),
        };
        let Ok(text) = row.get_ref(2).map(|v| match v {
            rusqlite::types::ValueRef::Text(t) | rusqlite::types::ValueRef::Blob(t) => {
                String::from_utf8_lossy(t)
            }
            _ => std::borrow::Cow::Borrowed(""),
        }) else {
            continue;
        };
        let (nesting, selects) = sql_nesting(&text);
        let name = || {
            row.get::<_, String>(1)
                .unwrap_or_else(|_| "<unnamed>".to_string())
        };
        if nesting > MAX_SQL_NESTING {
            return Err(format!(
                "refusing to open this database: schema entry `{}` nests {nesting} levels deep \
                 (the limit is {MAX_SQL_NESTING}); parsing it could overflow the stack and end \
                 the app",
                name()
            ));
        }
        if selects > MAX_SQL_SELECTS {
            return Err(format!(
                "refusing to open this database: schema entry `{}` nests {selects} SELECTs (the \
                 limit is {MAX_SQL_SELECTS})",
                name()
            ));
        }
        if view_definition(&sql_words(&text)).is_some() {
            view_bytes = view_bytes.saturating_add(text.len());
            if view_bytes > MAX_SCHEMA_VIEW_TEXT_BYTES {
                return Err(format!(
                    "refusing to open this database: its views' definitions exceed \
                     {MAX_SCHEMA_VIEW_TEXT_BYTES} bytes, more than the check of how they chain \
                     will hold"
                ));
            }
            views.push((text.into_owned(), nesting.max(selects) + 1));
        }
    }
    let view_nesting = deepest_view_chain(&views);
    if view_nesting > MAX_SCHEMA_VIEW_NESTING {
        return Err(format!(
            "refusing to open this database: its views nest up to {view_nesting} levels when \
             chained (the limit is {MAX_SCHEMA_VIEW_NESTING}); expanding them could overflow the \
             stack and end the app"
        ));
    }
    Ok(())
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
/// an unterminated comment or quote runs to the end of input, as in `SQLite`). U+FEFF where a token
/// could start is whitespace, and `$` inside a name is part of the name.
#[cfg(feature = "sqlite")]
struct SqlScan {
    /// The SQL with every comment and every U+FEFF `SQLite` skips replaced by one space; quoted
    /// spans kept verbatim.
    cleaned: String,
    /// `cleaned` with the CONTENT of every quoted span replaced by `_` (the delimiters are
    /// kept), so a search for `;` or `=` only ever sees code.
    masked: String,
}

/// `SQLite`'s `IdChar`: every byte >= 0x80 is one, whatever Unicode says of the character.
#[cfg(feature = "sqlite")]
fn is_sqlite_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$' || !c.is_ascii()
}

/// `sqlite3Isspace`: ASCII only, and unlike `char::is_ascii_whitespace` it includes `\x0b`.
#[cfg(feature = "sqlite")]
fn is_sqlite_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')
}

/// Whether what follows a `$ : @ #` gives it a name: `SQLite` needs one identifier character,
/// after any number of `::`.
#[cfg(feature = "sqlite")]
fn parameter_name_follows(mut rest: std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    while let Some(c) = rest.next() {
        if is_sqlite_id_char(c) {
            return true;
        }
        if c != ':' || rest.next() != Some(':') {
            return false;
        }
    }
    false
}

#[cfg(feature = "sqlite")]
fn scan_sql(sql: &str) -> SqlScan {
    let mut cleaned = String::with_capacity(sql.len());
    let mut masked = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    // Whether the character before this one belongs to a bare name or number. Every byte of U+FEFF
    // and `$` is an identifier character, so what they mean depends on it.
    let mut in_word = false;
    while let Some(c) = chars.next() {
        let after_word = std::mem::replace(&mut in_word, false);
        match c {
            '\u{feff}' if !after_word => {
                cleaned.push(' ');
                masked.push(' ');
            }
            // A named parameter (`$a`, `:a`, `@a`, `#a`). SQLite's tokenizer also accepts a
            // Tcl-style `$a(…)` suffix and reads it as part of the SAME token up to `)` or
            // whitespace, so a quote inside it does not open a string. Treating that quote as
            // one let `SELECT $a(') ; DELETE …'` pass as a single statement (round-4 review).
            '$' | ':' | '@' | '#'
                if !(c == '$' && after_word) && parameter_name_follows(chars.clone()) =>
            {
                cleaned.push(c);
                masked.push(c);
                while let Some(&n) = chars.peek() {
                    let continues =
                        is_sqlite_id_char(n) || (n == ':' && chars.clone().nth(1) == Some(':'));
                    if !continues {
                        break;
                    }
                    chars.next();
                    cleaned.push(n);
                    masked.push(n);
                    if n == ':' {
                        chars.next();
                        cleaned.push(n);
                        masked.push(n);
                    }
                }
                if chars.peek() == Some(&'(') {
                    while let Some(&n) = chars.peek() {
                        if is_sqlite_space(n) {
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
                in_word = is_sqlite_id_char(c);
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
                "query_db: pragma_function_list unavailable ({e}); allowing only SQLite's built-in functions"
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
                && ["sqlite", "sqlite3", "db", "db3", "sdb"]
                    .iter()
                    .any(|known| ext.eq_ignore_ascii_case(known))
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
/// the app's data. Names an app could plausibly use for its own file (`history`, `cookies`,
/// `favicons`) are left out: Chromium's have no extension, so discovery never lists them, and
/// anything under an engine directory is excluded by [`WEBVIEW_DIR_NAMES`] anyway.
#[cfg(feature = "sqlite")]
const WEBVIEW_DB_BASENAMES: &[&str] = &[
    "quotamanager",
    "web data",
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
    // WebKitGTK (Linux) writes its stores straight into the app data dir, next to the app's
    // own database — not under a `webkit` directory as WKWebView does — so these are named.
    "hsts-storage",
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
    // WebKitGTK's own directories inside the app data dir (Chromium spells it "local storage").
    "localstorage",
    "resourceloadstatistics",
    "mediakeys",
    "serviceworkers",
    "itp",
    "deviceidhashsalts",
    "cachestorage",
];

/// `WebKitGTK` directory names too generic to match anywhere: an engine store only as a DIRECT
/// child of a searched data dir (`WebKitGTK`'s `WebSQL` `databases/`), where an app's own nested
/// `databases/` folder elsewhere stays an app candidate.
#[cfg(feature = "sqlite")]
const WEBVIEW_ROOT_DIR_NAMES: &[&str] = &["databases"];

/// Whether a discovered database path is a `WebView`/browser-engine internal store rather
/// than the application's own database (audit / red-team "wrong DB" finding).
#[cfg(feature = "sqlite")]
#[must_use]
pub fn is_webview_internal(path: &Path) -> bool {
    is_webview_internal_under(path, None)
}

/// [`is_webview_internal`] for a database found under the search `root`: directory names are
/// matched only BELOW the root — a root that itself sits under a folder named `WebKit` or
/// `LocalStorage` must not turn every app database into an "engine internal" — and the
/// root-only names ([`WEBVIEW_ROOT_DIR_NAMES`]) apply to its direct children.
#[cfg(feature = "sqlite")]
fn is_webview_internal_under(path: &Path, root: Option<&Path>) -> bool {
    if let Some(name) = path.file_stem().and_then(|n| n.to_str()) {
        let name = name.to_ascii_lowercase();
        if WEBVIEW_DB_BASENAMES.iter().any(|n| name == *n) {
            return true;
        }
    }
    let relative = root.and_then(|r| path.strip_prefix(r).ok());
    let dirs = relative.unwrap_or(path).parent().unwrap_or(Path::new(""));
    let lower = |c: std::path::Component<'_>| c.as_os_str().to_string_lossy().to_ascii_lowercase();
    if dirs
        .components()
        .any(|c| WEBVIEW_DIR_NAMES.contains(&lower(c).as_str()))
    {
        return true;
    }
    relative.is_some()
        && dirs
            .components()
            .next()
            .is_some_and(|c| WEBVIEW_ROOT_DIR_NAMES.contains(&lower(c).as_str()))
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

/// Whether `path` can be a `SQLite` database by its first bytes: an extension alone also matches
/// Windows' `Thumbs.db` and other formats. An empty file is a new database to `SQLite`, and a file
/// that cannot be read here is given the benefit of the doubt (opening it reports the real error).
#[cfg(feature = "sqlite")]
fn has_sqlite_header(path: &Path) -> bool {
    use std::io::Read as _;
    let mut header = [0u8; 16];
    let Ok(file) = std::fs::File::open(path) else {
        return true;
    };
    let mut read = 0;
    let mut limited = file.take(header.len() as u64);
    while let Ok(n) = limited.read(&mut header[read..]) {
        if n == 0 {
            break;
        }
        read += n;
    }
    read == 0 || header == *b"SQLite format 3\0"
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
            if !seen.insert(key) || !has_sqlite_header(&path) {
                continue;
            }
            let size_bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
            let webview_internal = is_webview_internal_under(&path, Some(dir));
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
    on_db_stack(|| {
        query_with_limits(
            db_path,
            sql,
            params,
            max_rows,
            QUERY_TIMEOUT,
            MAX_QUERY_RESULT_BYTES,
        )
    })
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

    let (nesting, selects) = sql_nesting(sql);
    if nesting > MAX_SQL_NESTING {
        return Err(format!(
            "query nests {nesting} levels deep (the limit is {MAX_SQL_NESTING})"
        ));
    }
    if selects > MAX_SQL_SELECTS {
        return Err(format!(
            "query nests {selects} SELECTs (the limit is {MAX_SQL_SELECTS})"
        ));
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

    check_schema_nesting(db_path)?;

    // `mut` only matters from rusqlite 0.41, where `progress_handler` takes `&mut self`.
    #[allow(unused_mut)]
    let mut conn = rusqlite::Connection::open_with_flags(
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

    // By index, not `column_names()`: that returns a `Vec` before rusqlite 0.41, an iterator after.
    let names = (0..stmt.column_count())
        .map(|i| stmt.column_name(i).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("query error: {e}"))?;
    let column_names = unique_column_keys(&names.iter().map(String::as_str).collect::<Vec<_>>());
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
    } else if error.sqlite_error_code() == Some(rusqlite::ffi::ErrorCode::TooBig) {
        format!(
            "{context}: a value is larger than the {MAX_QUERY_CELL_BYTES} byte limit per cell \
             (the whole query fails, including substr() of that value). Leave the column out, or \
             select length(column) or typeof(column) to look at it"
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

/// Whether `encoded`, the re-encoding of the JSON text `source`, holds the same bytes apart from
/// whitespace between tokens and their order. Re-encoding only sorts keys, drops duplicates and
/// reformats numbers and escapes, and the last three all change the byte counts.
#[cfg(feature = "sqlite")]
fn same_json_content(source: &str, encoded: &str) -> bool {
    let mut counts = [0i64; 256];
    let (mut in_string, mut escaped) = (false, false);
    for &b in source.as_bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
        } else if b == b'"' {
            in_string = true;
        } else if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
            continue;
        }
        counts[usize::from(b)] += 1;
    }
    for &b in encoded.as_bytes() {
        counts[usize::from(b)] -= 1;
    }
    counts.iter().all(|&c| c == 0)
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
        // JSON has no infinity: `json!` would make it null, which reads as SQL NULL.
        Ok(ValueRef::Real(f)) if f.is_infinite() => {
            let name = if f > 0.0 { "Infinity" } else { "-Infinity" };
            let v = serde_json::json!({ "__real": name });
            let n = json_len(&v);
            (v, n)
        }
        Ok(ValueRef::Real(f)) => {
            let v = serde_json::json!(f);
            let n = json_len(&v);
            (v, n)
        }
        Ok(ValueRef::Text(t)) => {
            // Borrowed (no copy) for valid UTF-8; a cell is at most MAX_QUERY_CELL_BYTES.
            let s = String::from_utf8_lossy(t);
            if matches!(s, std::borrow::Cow::Owned(_)) {
                // Replacement characters would silently change the value: flag it, with the
                // stored bytes alongside.
                use base64::Engine;
                let v = serde_json::json!({
                    "__invalid_utf8": true,
                    "text": s,
                    "base64": base64::engine::general_purpose::STANDARD.encode(t),
                });
                let n = json_len(&v);
                return (n <= budget).then_some((v, n));
            }
            // JSON text is decoded unless that changes data: big numbers, `1.10` and duplicate keys
            // stay the stored string. Key order and whitespace carry no data.
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&s)
                && (parsed.is_object() || parsed.is_array())
                && serde_json::to_string(&parsed)
                    .is_ok_and(|encoded| same_json_content(&s, &encoded))
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

        // The metadata budget is the one thing not under test here, and 3 s of wall clock can
        // run out on a loaded machine.
        let r = db_health_report_budgeted(
            path.to_str().unwrap(),
            Duration::from_secs(120),
            Duration::ZERO,
            Duration::ZERO,
        )
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

    /// `n` nested FROM subqueries over `users`.
    fn nested_select(n: usize) -> String {
        format!(
            "{}SELECT id FROM users{}",
            "SELECT * FROM (".repeat(n),
            ")".repeat(n)
        )
    }

    /// A database with `users` plus schema rows written as a crafted file would hold them. Going
    /// through `writable_schema` also means the writing connection never parses them.
    fn db_with_schema_rows(rows: &[(String, String)]) -> (tempfile::NamedTempFile, PathBuf) {
        let (file, path) = create_test_db();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA writable_schema=ON; BEGIN;")
            .unwrap();
        for (name, sql) in rows {
            conn.execute(
                "INSERT INTO sqlite_master(type, name, tbl_name, rootpage, sql) \
                 VALUES ('view', ?1, ?1, 0, ?2)",
                [name, sql],
            )
            .unwrap();
        }
        conn.execute_batch("COMMIT;").unwrap();
        (file, path)
    }

    fn deep_view(n: usize) -> Vec<(String, String)> {
        vec![(
            "deep".to_string(),
            format!("CREATE VIEW deep AS {}", nested_select(n)),
        )]
    }

    fn view_chain(n: usize) -> Vec<(String, String)> {
        (0..n)
            .map(|i| {
                let from = if i == 0 {
                    "users".to_string()
                } else {
                    format!("v{}", i - 1)
                };
                (
                    format!("v{i}"),
                    format!("CREATE VIEW v{i} AS SELECT id FROM {from}"),
                )
            })
            .collect()
    }

    /// Run one case in a child process: a stack overflow is not a panic, it ends the process.
    fn in_child(test: &str, case: &str) -> std::process::Output {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("database::tests::{test}"),
                "--nocapture",
                "--test-threads=1",
            ])
            .env("VICTAURI_TEST_DB_CASE", case)
            .output()
            .unwrap()
    }

    /// Deep SQL or a crafted schema used to overflow the blocking thread's 2 MiB stack and take
    /// the host process with it. Each must be a clean error, on the plain default-stack thread
    /// that `query` is called from. The schema cases are what an agent hits by doing nothing
    /// unusual: `SELECT count(*) FROM users` merely loads the schema.
    #[test]
    fn deep_nesting_is_refused_without_harming_the_process() {
        const TEST: &str = "deep_nesting_is_refused_without_harming_the_process";
        let Ok(case) = std::env::var("VICTAURI_TEST_DB_CASE") else {
            for case in [
                "deep_sql",
                "deep_sql_at_the_size_cap",
                "deep_view_query",
                "deep_view_health",
                "huge_deep_view_query",
                "huge_deep_view_health",
                "view_chain",
                "cte_chain_at_the_size_cap",
            ] {
                let out = in_child(TEST, case);
                assert!(
                    out.status.success(),
                    "{case}: {}\n{}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            return;
        };
        let count = "SELECT count(*) FROM users";
        let (_f, path);
        let err = match case.as_str() {
            "deep_sql" => {
                (_f, path) = create_test_db();
                query(&path, &nested_select(6400), &[], None).unwrap_err()
            }
            "cte_chain_at_the_size_cap" => {
                (_f, path) = create_test_db();
                let mut sql = String::from("WITH a0 AS (SELECT 1)");
                let mut i = 1;
                while sql.len() < MAX_QUERY_SQL_BYTES - 100 {
                    sql.push_str(&format!(",a{i} AS (SELECT * FROM a{})", i - 1));
                    i += 1;
                }
                sql.push_str(&format!(" SELECT * FROM a{}", i - 1));
                query(&path, &sql, &[], None).unwrap_err()
            }
            "deep_sql_at_the_size_cap" => {
                (_f, path) = create_test_db();
                let n = (MAX_QUERY_SQL_BYTES - 100) / "SELECT * FROM ()".len();
                query(&path, &nested_select(n), &[], None).unwrap_err()
            }
            "deep_view_query" => {
                (_f, path) = db_with_schema_rows(&deep_view(20_000));
                query(&path, count, &[], None).unwrap_err()
            }
            "deep_view_health" => {
                (_f, path) = db_with_schema_rows(&deep_view(20_000));
                let long = Duration::from_secs(30);
                db_health_report(path.to_str().unwrap(), long, long).unwrap_err()
            }
            "huge_deep_view_query" => {
                (_f, path) = db_with_schema_rows(&deep_view(400_000));
                query(&path, count, &[], None).unwrap_err()
            }
            "huge_deep_view_health" => {
                (_f, path) = db_with_schema_rows(&deep_view(400_000));
                let long = Duration::from_secs(30);
                db_health_report(path.to_str().unwrap(), long, long).unwrap_err()
            }
            "view_chain" => {
                (_f, path) = db_with_schema_rows(&view_chain(30_000));
                query(&path, "SELECT * FROM v29999", &[], None).unwrap_err()
            }
            other => panic!("unknown case {other}"),
        };
        assert!(err.contains("nest") || err.contains("too long"), "{err}");
    }

    /// The nesting checks must not touch ordinary databases and queries.
    #[test]
    fn ordinary_nesting_and_view_chains_still_work() {
        let (_f, path) = create_test_db();
        let r = query(&path, &nested_select(100), &[], None).unwrap();
        assert_eq!(r["row_count"], 3);
        // A compound at SQLite's own limit is a lot of SELECTs and perfectly ordinary.
        let compound = vec!["SELECT 1"; 500].join(" UNION ALL ");
        assert_eq!(
            query(&path, &compound, &[], Some(1000)).unwrap()["row_count"],
            500
        );
        // Parentheses in a string are not nesting, in a query or in a schema.
        let parens = "(".repeat(5000);
        assert!(query(&path, &format!("SELECT '{parens}'"), &[], None).is_ok());

        let mut rows = view_chain(50);
        rows.extend(deep_view(100));
        rows.push((
            "quoted".to_string(),
            format!("CREATE VIEW quoted AS SELECT '{parens}' AS p"),
        ));
        let (_g, path) = db_with_schema_rows(&rows);
        let r = query(&path, "SELECT * FROM v49", &[], None).unwrap();
        assert_eq!(r["row_count"], 3);
        let r = query(&path, "SELECT * FROM deep", &[], None).unwrap();
        assert_eq!(r["row_count"], 3);
        let r = query(&path, "SELECT length(p) AS n FROM quoted", &[], None).unwrap();
        assert_eq!(r["rows"][0]["n"], 5000);
        let long = Duration::from_secs(30);
        let health = db_health_report(path.to_str().unwrap(), long, long).unwrap();
        assert_eq!(health["integrity_check"], "ok");
    }

    /// Rewrite the `type` column of every `CREATE VIEW` row.
    fn set_view_rows_type(path: &Path, type_sql: &str) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA writable_schema=ON; UPDATE sqlite_master SET type = {type_sql} \
             WHERE sql LIKE 'CREATE VIEW%';"
        ))
        .unwrap();
    }

    /// `type='VIEW'` or a BLOB still makes a working view (`SQLite` compares the column ignoring
    /// case, as text), so the view budget cannot key on the exact text `view`.
    #[test]
    fn a_views_budget_does_not_depend_on_its_type_column() {
        for type_sql in ["'VIEW'", "'View'", "CAST('view' AS BLOB)"] {
            let (_small, path) = db_with_schema_rows(&view_chain(3));
            set_view_rows_type(&path, type_sql);
            let r = query(&path, "SELECT * FROM v2", &[], None).unwrap();
            assert_eq!(r["row_count"], 3, "{type_sql}: it must be a working view");

            let (_big, path) = db_with_schema_rows(&view_chain(30_000));
            set_view_rows_type(&path, type_sql);
            let err = check_schema_nesting(&path).expect_err(type_sql);
            assert!(err.contains("nest"), "{type_sql}: {err}");
        }
    }

    /// What is on one chain is what expanding a view has to walk. A big schema of views that do
    /// not build on each other costs nothing of the kind.
    #[test]
    fn many_unrelated_views_open() {
        let rows: Vec<(String, String)> = (0..3_000)
            .map(|i| {
                (
                    format!("u{i}"),
                    format!("CREATE VIEW u{i} AS SELECT (id + 1) AS n FROM users"),
                )
            })
            .collect();
        let (_f, path) = db_with_schema_rows(&rows);
        let r = query(&path, "SELECT * FROM u2999", &[], None).unwrap();
        assert_eq!(r["row_count"], 3);
    }

    /// However a link is spelled, it is a link: the budget must not be blind to it.
    #[test]
    fn view_chains_are_seen_through_every_spelling() {
        type Link = fn(&str) -> String;
        let spellings: [(&str, Link); 7] = [
            ("bare", |v| v.to_string()),
            ("upper", |v| v.to_uppercase()),
            ("double-quoted", |v| format!("\"{v}\"")),
            ("bracketed", |v| format!("[{v}]")),
            ("backticked", |v| format!("`{v}`")),
            ("schema-qualified", |v| format!("main.{v}")),
            // `SQLite` reads U+FEFF at the start of a token as whitespace.
            ("byte-order-mark", |v| format!("\u{feff}{v}")),
        ];
        for (what, link) in spellings {
            let rows: Vec<(String, String)> = (0..2_500)
                .map(|i| {
                    let from = if i == 0 {
                        "users".to_string()
                    } else {
                        link(&format!("v{}", i - 1))
                    };
                    let sql = format!("CREATE VIEW v{i} AS SELECT id FROM {from}");
                    (format!("v{i}"), sql)
                })
                .collect();
            let (_f, path) = db_with_schema_rows(&rows);
            let err = check_schema_nesting(&path).expect_err(what);
            assert!(err.contains("nest"), "{what}: {err}");
        }
    }

    /// Two spellings `SQLite` accepts that the scanner read differently: U+FEFF starting a token is
    /// whitespace to `SQLite`, and `$` inside a name is part of the name, not a parameter. Each hid
    /// a whole chain from the budget.
    #[test]
    fn views_spelled_with_a_byte_order_mark_or_a_dollar_are_seen() {
        type Definition = fn(usize) -> String;
        let spellings: [(&str, Definition); 2] = [
            ("byte order mark before VIEW", |i| {
                let from = if i == 0 {
                    "users".to_string()
                } else {
                    format!("\u{feff}v{}", i - 1)
                };
                format!("CREATE \u{feff}VIEW v{i} AS SELECT id FROM {from}")
            }),
            ("dollar in the name", |i| {
                if i == 0 {
                    "CREATE VIEW v$0(' -- ') AS SELECT id FROM users".to_string()
                } else {
                    format!(
                        "CREATE VIEW v${i}(' -- ') AS SELECT \" -- \" FROM v${}",
                        i - 1
                    )
                }
            }),
        ];
        for (what, definition) in spellings {
            // SQLite itself takes this spelling, and it is a working chain.
            let (_small, small) = create_test_db();
            let conn = rusqlite::Connection::open(&small).unwrap();
            for i in 0..5 {
                conn.execute_batch(&definition(i)).expect(what);
            }
            let last = if what.contains("dollar") { "v$4" } else { "v4" };
            conn.prepare(&format!("SELECT * FROM \"{last}\""))
                .expect(what);

            let rows: Vec<(String, String)> = (0..2_500)
                .map(|i| (format!("row{i}"), definition(i)))
                .collect();
            let (_f, path) = db_with_schema_rows(&rows);
            let err = check_schema_nesting(&path).expect_err(what);
            assert!(err.contains("nest"), "{what}: {err}");
        }
    }

    /// The same two misreadings, where they meet the guards on an agent's query.
    #[test]
    fn the_scanner_tokenizes_a_byte_order_mark_and_a_dollar_as_sqlite_does() {
        let ctes: Vec<String> = (0..3_000)
            .map(|i| format!("c{i} AS (\u{feff}SELECT 1)"))
            .collect();
        let sql = format!("WITH {} SELECT 1", ctes.join(","));
        assert_eq!(sql_nesting(&sql).1, 3_001);
        assert!(is_read_only("\u{feff}SELECT 1"));
        assert_eq!(statement_count("\u{feff}SELECT 1;\u{feff}"), 1);
        // Only at the start of a token: inside a name it is a name character.
        let texts =
            |sql: &str| -> Vec<String> { sql_words(sql).into_iter().map(|w| w.text).collect() };
        assert_eq!(texts("a\u{feff}b"), ["a\u{feff}b"]);
        assert_eq!(texts("\u{feff}\u{feff}a b"), ["a", "b"]);

        // A name holding `$`: the `;` is inside a string.
        assert_eq!(statement_count("SELECT a$b('x y;z') FROM t"), 1);
        assert_eq!(statement_count("SELECT a$b(1); SELECT 2"), 2);
        // A real parameter keeps its Tcl-style suffix, which swallows the quote, so the `;` counts:
        // after a byte order mark, a string or a comment too. SQLite agrees.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for two in [
            "SELECT $a(') ; SELECT 2 --'",
            "SELECT \u{feff}$a(') ; SELECT 2 --'",
            "SELECT 'x',$a(') ; SELECT 2 --'",
            "SELECT 1,/* c */$a(') ; SELECT 2 --'",
        ] {
            assert_eq!(statement_count(two), 2, "{two}");
            // Only the first statement: from rusqlite 0.41 `prepare` refuses a trailing one.
            let (head, _) = two.split_once(';').expect(two);
            let first = conn.prepare(head).expect(two);
            assert_eq!(first.parameter_name(1), Some("$a(')"), "{two}");
        }
        // Straight after a string or a comment, too: neither is a word for `$` to continue.
        assert_eq!(statement_count("SELECT 'x'$a(') ; SELECT 2 --'"), 2);
        assert_eq!(statement_count("SELECT 1/* c */$a(') ; SELECT 2 --'"), 2);
        // Inside a name, a byte order mark does not end it, so `$b` is no parameter.
        assert_eq!(statement_count("SELECT a\u{feff}$b('x y;z') FROM t"), 1);
    }

    /// A true chain of 1,500 views, each also naming the next in a string, read as a cycle: an edge
    /// into a view still open charged nothing, and starting in the middle halved the bill.
    #[test]
    fn mentions_that_close_a_cycle_cannot_halve_a_chain() {
        let n = 1_500;
        let definition = |i: usize| {
            let from = if i == 0 {
                "users".to_string()
            } else {
                format!("v{}", i - 1)
            };
            format!(
                "CREATE VIEW v{i} AS SELECT id, 'v{}' AS note FROM {from}",
                i + 1
            )
        };
        let mut order = vec![n / 2];
        order.extend((0..n).filter(|i| *i != n / 2));
        let rows: Vec<(String, String)> = order
            .iter()
            .map(|i| (format!("v{i}"), definition(*i)))
            .collect();
        let views: Vec<(String, usize)> = rows.iter().map(|(_, sql)| (sql.clone(), 2)).collect();
        assert!(
            deepest_view_chain(&views) >= 2 * n,
            "{}",
            deepest_view_chain(&views)
        );
        let (_f, path) = db_with_schema_rows(&rows);
        let err = check_schema_nesting(&path).unwrap_err();
        assert!(err.contains("nest"), "{err}");
    }

    /// Review: a hub view `data` over 500 shards that each have a column `data` was charged as one
    /// cycle (2,001), and a healthy database refused: a column name counted as a link back.
    #[test]
    fn a_column_named_like_a_view_is_not_a_link() {
        let (_f, path) = create_test_db();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE t(id INTEGER, data TEXT)")
            .unwrap();
        for i in 0..500 {
            conn.execute_batch(&format!(
                "CREATE VIEW s{i}(id, data) AS SELECT id, COALESCE(TRIM(data), 0) AS data FROM t \
                 WHERE data IS NOT NULL ORDER BY data"
            ))
            .unwrap();
        }
        let shards: Vec<String> = (0..500).map(|i| format!("SELECT * FROM s{i}")).collect();
        conn.execute_batch(&format!(
            "CREATE VIEW data AS {}",
            shards.join(" UNION ALL ")
        ))
        .unwrap();
        conn.prepare("SELECT count(*) FROM data").unwrap();
        drop(conn);
        check_schema_nesting(&path).unwrap();
        query(&path, "SELECT count(*) AS n FROM data", &[], None).unwrap();
    }

    /// Every clause that cannot name a table: none of these views selects from `b`.
    #[test]
    fn only_the_places_that_name_a_table_are_links() {
        for sql in [
            "SELECT b FROM t",
            "SELECT t.b, 'b' AS note, \"b\" FROM t",
            "SELECT COALESCE(b, (b)) FROM t",
            "SELECT 1 FROM t WHERE b > 0 GROUP BY b HAVING b ORDER BY b LIMIT b",
            "SELECT 1 FROM t JOIN u ON t.b = u.b",
            "SELECT 1 FROM t JOIN u USING (b)",
            "SELECT 1 WHERE b",
            "SELECT x IN (b) FROM t",
            "WITH c(b) AS (SELECT 1) SELECT * FROM c",
        ] {
            let views = [
                (format!("CREATE VIEW a AS {sql}"), 2),
                ("CREATE VIEW b AS SELECT 1".to_string(), 2),
            ];
            assert_eq!(deepest_view_chain(&views), 2, "{sql}");
        }
        let columns = [
            ("CREATE VIEW a(b) AS SELECT 1".to_string(), 2),
            ("CREATE VIEW b AS SELECT 1".to_string(), 2),
        ];
        assert_eq!(deepest_view_chain(&columns), 2);
    }

    /// Everything `SQLite` itself expands as a table, wherever it stands in the statement: each
    /// spelling works as a chain, and a long one of it is refused. `FROM` is not the only way in
    /// (`x IN v`), the words that end a result list can be quoted, and tables follow a join
    /// condition.
    #[test]
    fn a_table_named_anywhere_is_still_a_link() {
        type Definition = fn(&str) -> String;
        let spellings: [(&str, Definition); 16] = [
            ("IN a view", |v| format!("SELECT 1 IN {v}")),
            ("NOT IN a schema's view", |v| {
                format!("SELECT 1 NOT IN main.{v}")
            }),
            ("IN a string", |v| format!("SELECT 1 IN '{v}'")),
            ("IN in a function", |v| format!("SELECT abs(1 IN {v})")),
            ("IN in WHERE", |v| format!("SELECT 1 WHERE 1 IN {v}")),
            ("scalar subquery", |v| {
                format!("SELECT (SELECT 1 FROM {v}) AS x")
            }),
            ("from a string", |v| format!("SELECT 1 FROM '{v}'")),
            ("subquery in WHERE", |v| {
                format!("SELECT 1 WHERE (SELECT 1 FROM {v})")
            }),
            ("compound", |v| format!("SELECT 1 UNION SELECT 1 FROM {v}")),
            ("values compound", |v| {
                format!("VALUES(1) UNION ALL SELECT 1 FROM {v}")
            }),
            ("quoted select before the table", |v| {
                format!("SELECT 1 AS \"select\" FROM users, {v}")
            }),
            ("a table named select", |v| {
                format!("SELECT 1 FROM users AS \"select\", {v}")
            }),
            ("after a join condition", |v| {
                format!("SELECT 1 FROM users a JOIN users b ON a.id = b.id, {v}")
            }),
            ("join after a join condition", |v| {
                format!("SELECT 1 FROM users a JOIN users b ON a.id = b.id LEFT JOIN {v}")
            }),
            ("after USING", |v| {
                format!("SELECT 1 FROM users a JOIN users b USING (id), {v}")
            }),
            ("parenthesised join", |v| {
                format!("SELECT 1 FROM (users a JOIN users b ON a.id IN (1, 2), {v})")
            }),
        ];
        for (what, body) in spellings {
            let definition = |i: usize| {
                if i == 0 {
                    "CREATE VIEW v0 AS SELECT id FROM users".to_string()
                } else {
                    format!("CREATE VIEW v{i} AS {}", body(&format!("v{}", i - 1)))
                }
            };
            // SQLite itself takes this spelling, and it is a working chain.
            let rows: Vec<(String, String)> =
                (0..5).map(|i| (format!("v{i}"), definition(i))).collect();
            let (_small, small) = db_with_schema_rows(&rows);
            let conn = rusqlite::Connection::open(&small).unwrap();
            let run = conn.prepare("SELECT * FROM v4");
            assert!(run.is_ok(), "{what}: {:?}", run.err());
            drop(run);
            drop(conn);

            let chain: Vec<(String, usize)> = (0..1_000).map(|i| (definition(i), 2)).collect();
            assert_eq!(deepest_view_chain(&chain), 2_000, "{what}");
        }
    }

    /// [`table_mentions`] takes these bare words for keywords, so a view must not be able to use
    /// one as a name, or a name after it would go unread.
    #[test]
    fn the_keywords_that_end_a_table_list_are_reserved() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for word in [
            "select", "from", "join", "where", "group", "having", "order", "limit", "on", "using",
            "in",
        ] {
            assert!(
                conn.execute_batch(&format!("CREATE TABLE {word}(x)"))
                    .is_err(),
                "{word}"
            );
        }
    }

    /// The tokenizer behind the chain check does not read parameters as `SQLite` does, and need
    /// not: `SQLite` refuses a whole schema holding a view with one, whatever follows.
    #[test]
    fn a_view_holding_a_parameter_does_not_load() {
        for body in [
            "SELECT 1 FROM users a JOIN users b ON :select, users c",
            "SELECT $a(') FROM users --'",
        ] {
            let rows = vec![("p".to_string(), format!("CREATE VIEW p AS {body}"))];
            let (_f, path) = db_with_schema_rows(&rows);
            let conn = rusqlite::Connection::open(&path).unwrap();
            let err = conn.prepare("SELECT * FROM users").err();
            assert!(
                format!("{err:?}").contains("parameters are not allowed in views"),
                "{body}: {err:?}"
            );
        }
    }

    #[test]
    fn a_view_chain_is_charged_for_every_view_it_can_reach() {
        let views = |sql: &[&str]| -> Vec<(String, usize)> {
            sql.iter().map(|s| ((*s).to_string(), 2)).collect()
        };
        // A cycle group costs all of its members, once, plus what lies below and above it.
        let group = views(&[
            "CREATE VIEW a AS SELECT * FROM b, d",
            "CREATE VIEW b AS SELECT * FROM a",
            "CREATE VIEW c AS SELECT * FROM a",
            "CREATE VIEW d AS SELECT 1",
            "CREATE VIEW lone AS SELECT 1",
        ]);
        assert_eq!(deepest_view_chain(&group), 8);
        // A view naming itself is not a longer chain.
        assert_eq!(
            deepest_view_chain(&views(&["CREATE VIEW c AS SELECT * FROM c"])),
            2
        );
        // A diamond counts one path, not both.
        let diamond = views(&[
            "CREATE VIEW top AS SELECT * FROM l, r",
            "CREATE VIEW l AS SELECT * FROM base",
            "CREATE VIEW r AS SELECT * FROM base",
            "CREATE VIEW base AS SELECT 1",
        ]);
        assert_eq!(deepest_view_chain(&diamond), 6);
        // Unrelated views are not a chain, however many.
        let many: Vec<(String, usize)> = (0..5_000)
            .map(|i| (format!("CREATE VIEW v{i} AS SELECT id FROM users"), 2))
            .collect();
        assert_eq!(deepest_view_chain(&many), 2);
    }

    /// The text is what gets parsed, so it names the view, and a definition may be spelled with a
    /// comment or `IF NOT EXISTS`.
    #[test]
    fn a_views_name_comes_from_its_definition() {
        let rows: Vec<(String, String)> = (0..2_500)
            .map(|i| {
                let from = if i == 0 {
                    "users".to_string()
                } else {
                    format!("v{}", i - 1)
                };
                let sql =
                    format!("CREATE /* c */ VIEW IF NOT EXISTS main.v{i} AS SELECT id FROM {from}");
                (format!("decoy{i}"), sql)
            })
            .collect();
        let (_f, path) = db_with_schema_rows(&rows);
        let err = check_schema_nesting(&path).unwrap_err();
        assert!(err.contains("nest"), "{err}");
    }

    /// `SQLite` refuses a circular view when it is used. Charging one must terminate.
    #[test]
    fn circular_views_do_not_hang_the_check() {
        let rows = vec![
            (
                "a".to_string(),
                "CREATE VIEW a AS SELECT * FROM b".to_string(),
            ),
            (
                "b".to_string(),
                "CREATE VIEW b AS SELECT * FROM a".to_string(),
            ),
            (
                "c".to_string(),
                "CREATE VIEW c AS SELECT * FROM c".to_string(),
            ),
        ];
        let (_f, path) = db_with_schema_rows(&rows);
        check_schema_nesting(&path).unwrap();
        let err = query(&path, "SELECT * FROM a", &[], None).unwrap_err();
        assert!(err.contains("circular"), "{err}");
    }

    /// The text of every view has to be held to work out the chains, so it is bounded.
    #[test]
    fn the_views_total_text_is_bounded() {
        let filler = "x".repeat(MAX_QUERY_SQL_BYTES - 200);
        let rows: Vec<(String, String)> = (0..MAX_SCHEMA_VIEW_TEXT_BYTES / filler.len() + 2)
            .map(|i| {
                let sql = format!("CREATE VIEW big{i} AS SELECT '{filler}' AS p");
                (format!("big{i}"), sql)
            })
            .collect();
        let (_f, path) = db_with_schema_rows(&rows);
        // On a busy machine, reading this much can run out the check's own budget first.
        let err = check_schema_nesting(&path).unwrap_err();
        assert!(
            err.contains("refusing") || err.contains("timed out"),
            "{err}"
        );
    }

    /// `selected_x` is a column, not a SELECT: 2,500 of them in one view are not nesting.
    #[test]
    fn identifiers_containing_select_are_not_counted() {
        let columns = (0..2_500)
            .map(|i| format!("1 AS selected_{i}"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(sql_nesting(&format!("SELECT {columns}")).1, 1);
        assert_eq!(sql_nesting("SELECT 1 UNION select 2").1, 2);
        assert_eq!(sql_nesting("SELECT 'select' AS \"select\"").1, 1);
    }

    /// A `.db` that is not `SQLite` (Windows' `Thumbs.db`) must not beat a real database by size.
    #[test]
    fn auto_selection_skips_files_that_are_not_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Thumbs.db"), vec![7u8; 100_000]).unwrap();
        std::fs::write(dir.path().join("tiny.db"), b"short").unwrap();
        let real = dir.path().join("real.sqlite");
        write_sqlite(&real, 10);
        let dirs = [dir.path().to_path_buf()];
        assert_eq!(select_app_database(&dirs).unwrap(), real);
        assert_eq!(classify_databases(&dirs).len(), 1);
    }

    /// An empty file is what `SQLite` itself treats as a new database.
    #[test]
    fn auto_selection_keeps_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("new.db");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(
            select_app_database(&[dir.path().to_path_buf()]).unwrap(),
            empty
        );
    }

    #[test]
    fn discovery_matches_extensions_in_any_case_and_finds_db3() {
        let dir = tempfile::tempdir().unwrap();
        write_sqlite(&dir.path().join("MAIN.DB"), 10);
        write_sqlite(&dir.path().join("data.db3"), 10);
        assert_eq!(discover_databases(dir.path()).len(), 2);
    }

    /// Chromium's `History` has no extension and lives under the engine's directory; an app's
    /// own `history.db` next to its data is the app's.
    #[test]
    fn an_apps_own_history_db_is_not_an_engine_store() {
        let dir = tempfile::tempdir().unwrap();
        let history = dir.path().join("history.db");
        write_sqlite(&history, 1);
        rusqlite::Connection::open(&history)
            .unwrap()
            .execute_batch("INSERT INTO t (blob) VALUES (zeroblob(100000))")
            .unwrap();
        write_sqlite(&dir.path().join("analytics.db"), 1);
        assert_eq!(
            select_app_database(&[dir.path().to_path_buf()]).unwrap(),
            history
        );
        assert!(is_webview_internal(Path::new(
            "/app/EBWebView/Default/history.db"
        )));
    }

    fn one_value(path: &Path, sql: &str) -> serde_json::Value {
        let r = query(path, sql, &[], None).unwrap();
        r["rows"][0]["v"].clone()
    }

    /// A value comes back as it is stored: JSON text is decoded only when that loses nothing.
    #[test]
    fn query_values_are_returned_faithfully() {
        let (_f, path) = create_test_db();
        for text in [
            r#"{"id":123456789012345678901234567890}"#,
            r#"{"id2":1.10}"#,
            r#"{"a":2,"a":3}"#,
            r#"{"e":1e3}"#,
        ] {
            assert_eq!(
                one_value(&path, &format!("SELECT '{text}' AS v")),
                serde_json::json!(text),
                "{text}"
            );
        }
        // JSON that survives the trip is still decoded. Key order and whitespace carry no data, and
        // real JSON columns have both, so neither may turn a column into a string.
        assert_eq!(
            one_value(&path, r#"SELECT '{"a":1,"b":[2,"x"]}' AS v"#),
            serde_json::json!({"a": 1, "b": [2, "x"]})
        );
        assert_eq!(
            one_value(&path, r#"SELECT '{"theme":"dark","lang":"en"}' AS v"#),
            serde_json::json!({"theme": "dark", "lang": "en"})
        );
        assert_eq!(
            one_value(&path, r#"SELECT '{ "z" : [1, 2], "a b" : "c  d" }' AS v"#),
            serde_json::json!({"z": [1, 2], "a b": "c  d"})
        );
        assert_eq!(
            one_value(&path, "SELECT ' [1] ' AS v"),
            serde_json::json!([1])
        );
        // Infinities are not SQL NULL.
        assert_eq!(
            one_value(&path, "SELECT 9e999 AS v"),
            serde_json::json!({"__real": "Infinity"})
        );
        assert_eq!(
            one_value(&path, "SELECT -9e999 AS v"),
            serde_json::json!({"__real": "-Infinity"})
        );
        assert_eq!(
            one_value(&path, "SELECT NULL AS v"),
            serde_json::Value::Null
        );
        // Bytes that are not UTF-8 are flagged and still recoverable.
        assert_eq!(
            one_value(&path, "SELECT CAST(x'ff41' AS TEXT) AS v"),
            serde_json::json!({"__invalid_utf8": true, "text": "\u{fffd}A", "base64": "/0E="})
        );
    }

    /// One oversized cell fails the row: the error must say what happened and the way forward.
    #[test]
    fn an_oversized_cell_error_names_the_cap_and_the_way_forward() {
        let (_f, path) = create_test_db();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE docs (body BLOB); INSERT INTO docs VALUES (zeroblob(3000000));",
        )
        .unwrap();
        drop(conn);
        let err = query(&path, "SELECT * FROM docs", &[], None).unwrap_err();
        assert!(err.contains("1048576"), "{err}");
        assert!(err.contains("length("), "{err}");
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
        hook: impl Fn(&rusqlite::Connection) + Send + Sync + 'static,
        f: impl FnOnce() -> T,
    ) -> T {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                ON_OPEN.with(|h| *h.borrow_mut() = None);
            }
        }
        ON_OPEN.with(|h| *h.borrow_mut() = Some(Arc::new(hook)));
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
        // One transaction: 400 autocommitted inserts cost a sync each (~75 s on a busy box).
        let mut ddl = String::from("BEGIN;");
        for i in 0..400 {
            ddl.push_str(&format!(
                "CREATE TABLE t{i} (x INTEGER); INSERT INTO t{i} VALUES (1);"
            ));
        }
        ddl.push_str("COMMIT;");
        conn.execute_batch(&ddl).unwrap();
        drop(conn);
        // Budgets far above need, so a loaded machine cannot fail this: it checks that hundreds
        // of tables are all listed and counted. The per-count cost that capped this at ~200
        // tables is pinned deterministically by `interrupt_guard_parks_instead_of_polling`.
        let r = db_health_report(
            file.path().to_str().unwrap(),
            Duration::from_secs(60),
            Duration::from_secs(60),
        )
        .unwrap();
        assert_eq!(r["tables"].as_array().unwrap().len(), 400);
        assert_eq!(r["row_counts_complete"], true, "{r}");
    }

    /// The watchdog used to sleep in fixed 25 ms steps that every guard's drop then joined —
    /// ~25 ms per guarded statement however fast it was, which capped `db_health` at ~200 table
    /// counts per 5 s budget. It must park until the deadline (woken early only by the drop).
    /// Counted, not timed, so machine load cannot make this flaky.
    #[test]
    fn interrupt_guard_parks_instead_of_polling() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let guard = InterruptGuard::arm(&conn, Duration::from_secs(60));
        let wakes = Arc::clone(&guard.wakes);
        std::thread::sleep(Duration::from_millis(300));
        drop(guard);
        let n = wakes.load(Ordering::Relaxed);
        // One wake on start + one on the drop's unpark; allow one spurious park return. A 25 ms
        // stepping watchdog would have woken ~12 times in 300 ms.
        assert!(
            n <= 3,
            "the watchdog woke {n} times in 300 ms: it is polling"
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
            "SELECT id, name, score, NULL AS n, X'00FF10' AS b, '{\"k\":[1,2]}' AS j, \
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
            // Refused per row BEFORE matching — the refusal itself is the proof. (A wall-clock
            // bound here never discriminated: with three short rows the match is instant either
            // way, and it only flaked on loaded machines.)
            let err = query(&path, sql, &[serde_json::json!(pattern)], None).unwrap_err();
            assert!(err.contains("pattern too complex"), "{sql}: {err}");
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
        // The query connection's own busy timeout — deterministic, unlike wall time.
        let pragma = query(&path, "PRAGMA busy_timeout", &[], None).unwrap();
        let configured = pragma["rows"][0]
            .as_object()
            .and_then(|row| row.values().next())
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| panic!("busy_timeout not reported: {pragma}"));
        assert_eq!(
            u128::from(configured),
            QUERY_BUSY_TIMEOUT.as_millis(),
            "{pragma}"
        );
        assert!(QUERY_BUSY_TIMEOUT <= Duration::from_secs(1));
        let locker = rusqlite::Connection::open(&path).unwrap();
        locker.execute_batch("BEGIN EXCLUSIVE").unwrap();
        // One wait, not one per connection or statement. Load only adds time, so the quickest of
        // three is what counts. Without usleep a build's busy handler sleeps in whole seconds.
        let bound = if cfg!(target_os = "macos") {
            Duration::from_secs(4)
        } else {
            // A second wait measured 3.5 s; one wait reached 1.94 s under a loaded test run.
            Duration::from_millis(2800)
        };
        let quickest = (0..3)
            .map(|_| {
                let started = Instant::now();
                let err = query(&path, "SELECT * FROM users", &[], None).unwrap_err();
                assert!(err.contains("locked"), "{err}");
                started.elapsed()
            })
            .min()
            .unwrap();
        assert!(quickest < bound, "lock wait took {quickest:?}");
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
               INSERT INTO config VALUES ('settings', '{"lang":"en","theme":"dark"}');"#,
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

    /// The same class, wherever the scanner's idea of the token differed from `SQLite`'s: every
    /// byte >= 0x80 is an identifier character there (`€` is not alphanumeric to Rust), a `::`
    /// continues any parameter name, and only ASCII whitespace ends the `(…)` suffix.
    #[test]
    fn the_scanner_reads_a_parameter_token_as_sqlite_does() {
        let (_f, path) = create_test_db();
        let conn = rusqlite::Connection::open(&path).unwrap();
        for sql in [
            "SELECT $\u{20ac}(') ; CREATE TABLE stacked(x) --'",
            "SELECT :\u{20ac}(') ; CREATE TABLE stacked(x) --'",
            "SELECT $a\u{20ac}(') ; CREATE TABLE stacked(x) --'",
            "SELECT @a::(') ; CREATE TABLE stacked(x) --'",
            "SELECT :a::b(') ; CREATE TABLE stacked(x) --'",
            "SELECT $a(\u{a0}') ; CREATE TABLE stacked(x) --'",
            "SELECT $a(\u{2003}') ; CREATE TABLE stacked(x) --'",
        ] {
            assert!(
                sqlite_runs_a_second_statement(&conn, sql),
                "SQLite's reading of {sql}"
            );
            let err = validate_query(sql).expect_err(sql);
            assert!(err.contains("stacked queries"), "{sql}: {err}");
        }
        for sql in [
            "SELECT '\u{20ac};' AS s",
            "SELECT 1 AS \u{20ac}",
            "SELECT :\u{20ac}",
            "SELECT $a::b FROM t",
        ] {
            assert_eq!(validate_query(sql), Ok(()), "must allow: {sql}");
        }
    }

    /// Whether `SQLite` itself runs the `CREATE TABLE stacked` at the end of `sql` as a statement.
    fn sqlite_runs_a_second_statement(conn: &rusqlite::Connection, sql: &str) -> bool {
        let _ = conn.execute_batch(sql);
        let ran = conn.prepare("SELECT * FROM stacked").is_ok();
        let _ = conn.execute_batch("DROP TABLE IF EXISTS stacked");
        ran
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
        // WebKitGTK keeps its stores in the app data dir itself (it outranked demo.db).
        assert!(is_webview_internal(Path::new(
            "/home/u/.local/share/com.victauri.demo/hsts-storage.sqlite"
        )));
        assert!(is_webview_internal(Path::new(
            "/home/u/.local/share/com.victauri.demo/localstorage/file__0.localstorage"
        )));
        assert!(!is_webview_internal(Path::new(
            "/home/u/.local/share/com.victauri.demo/demo.db"
        )));
        // Real application DBs are NOT flagged.
        assert!(!is_webview_internal(Path::new("/app/data/4da.db")));
        assert!(!is_webview_internal(Path::new("/app/data/app.sqlite")));
        assert!(!is_webview_internal(Path::new("/app/notes.db")));
        // More WebKitGTK stores it writes into the app data dir.
        let root = Path::new("/home/u/.local/share/com.x.app");
        for store in [
            "serviceworkers/ServiceWorkerRegistrations-8.sqlite3",
            "itp/observations.db",
            "deviceidhashsalts/x.db",
            "CacheStorage/x/records.db",
            "databases/Databases.db",
        ] {
            assert!(
                is_webview_internal_under(&root.join(store), Some(root)),
                "{store}"
            );
        }
        // ...but `databases` only as a direct child: an app's own nested folder is the app's.
        assert!(!is_webview_internal_under(
            &root.join("data/databases/app.db"),
            Some(root)
        ));
    }

    /// Engine directory names are matched only BELOW the searched root: a data dir that sits
    /// under a folder named `WebKit` / `LocalStorage` must not hide every database in it.
    #[test]
    fn engine_dir_names_above_the_search_root_do_not_count() {
        for root in [
            "/home/u/WebKit/myapp/data",
            "D:/work/LocalStorage/myapp/data",
        ] {
            let root = Path::new(root);
            assert!(!is_webview_internal_under(&root.join("app.db"), Some(root)));
            assert!(is_webview_internal_under(
                &root.join("IndexedDB/x.sqlite3"),
                Some(root)
            ));
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("WebKit").join("app");
        std::fs::create_dir_all(&root).unwrap();
        write_sqlite(&root.join("app.db"), 10);
        let picked = select_app_database(std::slice::from_ref(&root));
        assert!(
            picked.as_ref().is_ok_and(|p| p.ends_with("app.db")),
            "{picked:?}"
        );
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
