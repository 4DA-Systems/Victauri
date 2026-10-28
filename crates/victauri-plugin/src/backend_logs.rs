//! Backend (Rust-side) log capture — what the app prints to its debug console.
//!
//! Victauri's other log tools see the **webview** (JS console, fetch, IPC). This
//! module captures the **Rust backend's** own diagnostics so an agent can read
//! them with the same tool surface, filter them, digest them, and block on them
//! (`wait_for {condition:"log"}`).
//!
//! Three in-process sources feed one ring buffer:
//!
//! | Source | App change | What it sees |
//! |---|---|---|
//! | [`log_layer`] | one `.with(...)` on the app's `tracing` registry | every `tracing` event (and `log` records bridged by `tracing-log`) with structured fields + span context |
//! | [`log_logger`] / [`wrap_logger`] | one line where the app installs its `log` logger (`tauri-plugin-log`, `env_logger`, `fern`, …) | every `log` record |
//! | panic hook | **none** — installed by the plugin | every Rust panic, including panics on background threads that otherwise only reach stderr |
//!
//! A fourth, **out-of-process** source covers everything else — raw
//! `println!`/`eprintln!`, C libraries, crash messages, even the build output
//! of `tauri dev`: launch the app under `victauri run -- <cmd>`. The launcher
//! owns the app's stdout/stderr, so a crash's last words survive the crash (an
//! in-process capture dies with the process). The plugin reads that capture
//! through the `logs stdout` action.
//!
//! Capture is **debug-only**: in release builds [`log_layer`] returns `None`
//! (a no-op layer), [`wrap_logger`] returns the inner logger unchanged and the
//! panic hook is never installed.
//!
//! Design notes (measured, see `docs/src/backend-logs.md`): one captured event
//! costs a few microseconds in a debug build — orders of magnitude less than the
//! console write the app already pays for — and a waiter blocked in
//! `wait_for log` wakes within a fraction of a millisecond of the event.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// Default number of entries kept in the ring buffer.
pub const DEFAULT_CAPACITY: usize = 5_000;
/// Default byte budget for the ring buffer (approximate, counts text bytes).
pub const DEFAULT_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Longest message kept per entry; longer messages are truncated with a marker.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024;
/// Longest single field value kept per entry.
pub const MAX_FIELD_BYTES: usize = 2 * 1024;
/// Longest backtrace kept for a captured panic.
pub const MAX_BACKTRACE_BYTES: usize = 16 * 1024;

// ── Levels ──────────────────────────────────────────────────────────────────

/// Severity of a captured backend log entry (ordered: `Trace` < `Error`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// `trace!`
    Trace,
    /// `debug!`
    Debug,
    /// `info!`
    Info,
    /// `warn!`
    Warn,
    /// `error!` — and every captured panic.
    Error,
}

impl LogLevel {
    /// Parse a level name (`trace`/`debug`/`info`/`warn`/`warning`/`error`, any case).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "trace" => Some(Self::Trace),
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" | "warning" => Some(Self::Warn),
            "error" | "err" | "fatal" | "panic" => Some(Self::Error),
            _ => None,
        }
    }

    /// Lowercase name, as serialized.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    const fn from_tracing(level: tracing::Level) -> Self {
        match level {
            tracing::Level::TRACE => Self::Trace,
            tracing::Level::DEBUG => Self::Debug,
            tracing::Level::INFO => Self::Info,
            tracing::Level::WARN => Self::Warn,
            tracing::Level::ERROR => Self::Error,
        }
    }

    const fn to_level_filter(self) -> tracing::level_filters::LevelFilter {
        match self {
            Self::Trace => tracing::level_filters::LevelFilter::TRACE,
            Self::Debug => tracing::level_filters::LevelFilter::DEBUG,
            Self::Info => tracing::level_filters::LevelFilter::INFO,
            Self::Warn => tracing::level_filters::LevelFilter::WARN,
            Self::Error => tracing::level_filters::LevelFilter::ERROR,
        }
    }

    const fn from_log(level: log::Level) -> Self {
        match level {
            log::Level::Trace => Self::Trace,
            log::Level::Debug => Self::Debug,
            log::Level::Info => Self::Info,
            log::Level::Warn => Self::Warn,
            log::Level::Error => Self::Error,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Trace => 0,
            Self::Debug => 1,
            Self::Info => 2,
            Self::Warn => 3,
            Self::Error => 4,
        }
    }
}

/// Which capture path produced an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogSource {
    /// The `tracing` layer ([`log_layer`]).
    Tracing,
    /// The `log` crate adapter ([`log_logger`] / [`wrap_logger`]).
    Log,
    /// The panic hook.
    Panic,
}

// ── Entries ─────────────────────────────────────────────────────────────────

/// One captured backend log record.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct BackendLogEntry {
    /// Monotonic sequence number (use as a cursor: `since_seq`).
    pub seq: u64,
    /// Capture time, Unix epoch milliseconds.
    pub ts_ms: u64,
    /// Severity.
    pub level: LogLevel,
    /// Log target (usually the module path, or an explicit `target:`).
    pub target: String,
    /// The formatted message.
    pub message: String,
    /// Structured key/value fields (`elapsed_ms = 41191` → `{"elapsed_ms": 41191}`).
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    pub fields: serde_json::Map<String, serde_json::Value>,
    /// Active spans, outermost first (`name{k=v}`), when the source is `tracing`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<String>,
    /// Name of the emitting thread, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// `file:line` of the call site, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Which capture path produced this entry.
    pub source: LogSource,
}

impl BackendLogEntry {
    /// An info-level `tracing` entry (tests).
    #[cfg(test)]
    pub(crate) fn for_tests(target: &str, message: &str) -> Self {
        Self {
            seq: 0,
            ts_ms: now_ms(),
            level: LogLevel::Info,
            target: target.to_string(),
            message: message.to_string(),
            fields: serde_json::Map::new(),
            spans: Vec::new(),
            thread: None,
            location: None,
            source: LogSource::Tracing,
        }
    }

    fn approx_bytes(&self) -> usize {
        let fields: usize = self
            .fields
            .iter()
            .map(|(k, v)| k.len() + value_len(v))
            .sum();
        let spans: usize = self.spans.iter().map(String::len).sum();
        96 + self.target.len() + self.message.len() + fields + spans
    }

    /// One-line human rendering: `12:01:02.345 WARN target: message k=v`.
    #[must_use]
    pub fn render(&self) -> String {
        let secs = self.ts_ms / 1000;
        let ms = self.ts_ms % 1000;
        let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
        let mut out = format!(
            "{h:02}:{m:02}:{s:02}.{ms:03}Z {:<5} {}: {}",
            self.level.as_str().to_ascii_uppercase(),
            self.target,
            self.message
        );
        for (k, v) in &self.fields {
            match v {
                serde_json::Value::String(s) => {
                    let _ = write!(out, " {k}={s:?}");
                }
                other => {
                    let _ = write!(out, " {k}={other}");
                }
            }
        }
        out
    }
}

fn value_len(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::String(s) => s.len(),
        _ => 16,
    }
}

// ── Buffer ──────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Ring {
    entries: VecDeque<BackendLogEntry>,
    next_seq: u64,
    bytes: usize,
    /// Entries evicted to respect capacity (lifetime).
    evicted: u64,
    /// Every entry below this sequence number was removed by [`LogBuffer::clear`] — deliberately,
    /// so a cursor older than it is not an eviction gap.
    cleared_before: u64,
    /// Lifetime totals per level (index = `LogLevel::index`).
    totals: [u64; 5],
}

impl Ring {
    /// Index in `entries` of the first entry with `seq >= seq` (sequence numbers in the ring
    /// are contiguous, so this is arithmetic, not a scan).
    fn index_of(&self, seq: u64) -> usize {
        self.entries.front().map_or(0, |front| {
            usize::try_from(seq.saturating_sub(front.seq))
                .unwrap_or(usize::MAX)
                .min(self.entries.len())
        })
    }

    /// Entries with `seq >= from` that were evicted (not cleared) before they could be read.
    fn evicted_since(&self, from: u64) -> u64 {
        let oldest = self.entries.front().map_or(self.next_seq, |e| e.seq);
        oldest.saturating_sub(from.max(self.cleared_before))
    }
}

thread_local! {
    /// Whether this thread currently holds a [`LogBuffer`] lock (see [`LogBuffer::try_push`]).
    static HOLDS_RING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A held ring lock; clears [`HOLDS_RING`] when released.
struct RingGuard<'a>(std::sync::MutexGuard<'a, Ring>);

impl Drop for RingGuard<'_> {
    fn drop(&mut self) {
        HOLDS_RING.with(|h| h.set(false));
    }
}

impl std::ops::Deref for RingGuard<'_> {
    type Target = Ring;
    fn deref(&self) -> &Ring {
        &self.0
    }
}

impl std::ops::DerefMut for RingGuard<'_> {
    fn deref_mut(&mut self) -> &mut Ring {
        &mut self.0
    }
}

/// `format!("{value:?}")`, except formatting stops once `max` bytes are written: a huge
/// `Debug` value (a multi-megabyte buffer recorded as a field or by `#[instrument]`) then costs
/// at most `max` bytes instead of its whole size, on every event that carries it.
fn debug_bounded(value: &dyn std::fmt::Debug, max: usize) -> String {
    struct Bounded {
        out: String,
        max: usize,
        cut: bool,
    }
    impl std::fmt::Write for Bounded {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            let room = self.max.saturating_sub(self.out.len());
            if s.len() <= room {
                self.out.push_str(s);
                return Ok(());
            }
            let mut end = room;
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            self.out.push_str(&s[..end]);
            self.cut = true;
            // Stop the formatter: the rest would be discarded anyway.
            Err(std::fmt::Error)
        }
    }
    let mut b = Bounded {
        out: String::new(),
        max,
        cut: false,
    };
    let _ = write!(b, "{value:?}");
    if b.cut {
        b.out.push_str("…[truncated]");
    }
    b.out
}

thread_local! {
    /// Whether this thread is inside this module's own `Notify` machinery (waking waiters, or
    /// registering/polling a waiter). Tokio can emit `tracing` events from in there — with
    /// `tokio_unstable` + its `tracing` feature it traces every waker clone while holding the
    /// `Notify`'s waiter lock — and such an event reaches [`BackendLogLayer`], whose push would
    /// call `notify_waiters` again on the same lock: a self-deadlock that froze every logging
    /// thread of the app. A push made from in there still records its entry but skips the wake
    /// (the waiters being woken rescan anyway).
    static IN_NOTIFY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Sets [`IN_NOTIFY`] for its lifetime (restoring the previous value, also on unwind).
struct NotifyScope(bool);

impl NotifyScope {
    fn enter() -> Self {
        Self(IN_NOTIFY.with(|f| f.replace(true)))
    }
}

impl Drop for NotifyScope {
    fn drop(&mut self) {
        let previous = self.0;
        IN_NOTIFY.with(|f| f.set(previous));
    }
}

/// A future whose every poll runs inside a [`NotifyScope`] (for a `Notified` being awaited).
struct PollInNotifyScope<F>(F);

impl<F: std::future::Future + Unpin> std::future::Future for PollInNotifyScope<F> {
    type Output = F::Output;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let _scope = NotifyScope::enter();
        std::pin::Pin::new(&mut self.0).poll(cx)
    }
}

/// Bounded ring buffer of captured backend log entries.
///
/// A process-global instance ([`global`]) is fed by the capture sources and read
/// by the MCP tools; separate instances exist only for tests.
pub struct LogBuffer {
    ring: Mutex<Ring>,
    notify: tokio::sync::Notify,
    capacity: usize,
    max_bytes: usize,
}

impl std::fmt::Debug for LogBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogBuffer")
            .field("capacity", &self.capacity)
            .field("max_bytes", &self.max_bytes)
            .finish_non_exhaustive()
    }
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY, DEFAULT_MAX_BYTES)
    }
}

impl LogBuffer {
    /// A new, empty buffer holding at most `capacity` entries and roughly
    /// `max_bytes` of text (both clamped to at least 1).
    #[must_use]
    pub fn new(capacity: usize, max_bytes: usize) -> Self {
        Self {
            ring: Mutex::new(Ring::default()),
            notify: tokio::sync::Notify::new(),
            capacity: capacity.max(1),
            max_bytes: max_bytes.max(1),
        }
    }

    fn lock(&self) -> RingGuard<'_> {
        let guard = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        HOLDS_RING.with(|h| h.set(true));
        RingGuard(guard)
    }

    /// Append an entry (its `seq` is assigned here) and wake waiters. Dropped if this thread
    /// already holds the lock — only possible when a panic under the lock runs a hook that
    /// logs (the app's own panic hook, chained by Victauri's), which would otherwise deadlock.
    pub fn push(&self, entry: BackendLogEntry) {
        if HOLDS_RING.with(std::cell::Cell::get) {
            return;
        }
        let mut ring = self.lock();
        Self::push_locked(&mut ring, entry, self.capacity, self.max_bytes);
        drop(ring);
        self.wake_waiters();
    }

    /// Push used from the panic hook. A panic on a thread that is itself holding this lock
    /// (mid-push or mid-query) must not deadlock, so that one entry is dropped. Any other
    /// thread waits: holders only do bounded in-memory work and never log under the lock, and
    /// a `try_lock` dropped real panics whenever an agent query or another thread's log write
    /// happened to hold the lock at that moment.
    fn try_push(&self, entry: BackendLogEntry) -> bool {
        if HOLDS_RING.with(std::cell::Cell::get) {
            return false;
        }
        let mut ring = self.lock();
        Self::push_locked(&mut ring, entry, self.capacity, self.max_bytes);
        drop(ring);
        self.wake_waiters();
        true
    }

    fn push_locked(ring: &mut Ring, mut entry: BackendLogEntry, capacity: usize, max_bytes: usize) {
        entry.seq = ring.next_seq;
        ring.next_seq += 1;
        ring.totals[entry.level.index()] += 1;
        ring.bytes += entry.approx_bytes();
        ring.entries.push_back(entry);
        while ring.entries.len() > capacity || (ring.bytes > max_bytes && ring.entries.len() > 1) {
            if let Some(old) = ring.entries.pop_front() {
                ring.bytes = ring.bytes.saturating_sub(old.approx_bytes());
                ring.evicted += 1;
            }
        }
    }

    /// Wake `wait_for` waiters — unless this thread is already inside the `Notify` machinery
    /// (see [`IN_NOTIFY`]), where waking again would deadlock.
    fn wake_waiters(&self) {
        if IN_NOTIFY.with(std::cell::Cell::get) {
            return;
        }
        let _scope = NotifyScope::enter();
        self.notify.notify_waiters();
    }

    /// The sequence number the next entry will get (a cursor for "from now on").
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.lock().next_seq
    }

    /// The first sequence number whose entry was captured at or after `ts_ms`
    /// (i.e. a cursor for "everything since that moment"); `next_seq` when none.
    #[must_use]
    pub fn seq_at_or_after(&self, ts_ms: u64) -> u64 {
        let ring = self.lock();
        ring.entries
            .iter()
            .find(|e| e.ts_ms >= ts_ms)
            .map_or(ring.next_seq, |e| e.seq)
    }

    /// Run a query against the buffer.
    #[must_use]
    pub fn query(&self, q: &LogQuery) -> LogPage {
        let ring = self.lock();
        let oldest_seq = ring.entries.front().map_or(ring.next_seq, |e| e.seq);
        let matcher = q.matcher();
        // Newest-first scan so `limit` keeps the most recent matches, then flip.
        let mut matched: Vec<BackendLogEntry> = Vec::new();
        let mut total_matched = 0usize;
        for e in ring.entries.iter().rev() {
            if let Some(since) = q.since_seq
                && e.seq < since
            {
                break;
            }
            if !matcher.matches(e) {
                continue;
            }
            total_matched += 1;
            if matched.len() < q.limit {
                matched.push(e.clone());
            }
        }
        matched.reverse();
        let gap = q.since_seq.is_some_and(|s| ring.evicted_since(s) > 0);
        LogPage {
            truncated: total_matched > matched.len(),
            total_matched,
            entries: matched,
            next_seq: ring.next_seq,
            oldest_seq,
            evicted: ring.evicted,
            gap,
        }
    }

    /// Entries with `seq` in `[from, to)` at or above `min_level`, capped at `limit`
    /// (newest kept). Used to attach "what the backend logged during this call".
    #[must_use]
    pub fn range(
        &self,
        from: u64,
        to: u64,
        min_level: LogLevel,
        limit: usize,
    ) -> (Vec<BackendLogEntry>, usize) {
        let ring = self.lock();
        let all: Vec<&BackendLogEntry> = ring
            .entries
            .range(ring.index_of(from)..)
            .take_while(|e| e.seq < to)
            .filter(|e| e.level >= min_level)
            .collect();
        let total = all.len();
        let start = total.saturating_sub(limit);
        (all[start..].iter().map(|e| (*e).clone()).collect(), total)
    }

    /// Aggregate view: counts, noisiest targets, repeated message templates,
    /// recent warnings/errors and every captured panic — the cheap first read
    /// before paging raw entries.
    #[must_use]
    pub fn digest(&self, recent: usize, top: usize) -> LogDigest {
        let ring = self.lock();
        let mut in_buffer = [0u64; 5];
        let mut targets: HashMap<&str, u64> = HashMap::new();
        let mut templates: HashMap<(LogLevel, String, String), TemplateStat> = HashMap::new();
        let mut recent_problems: VecDeque<BackendLogEntry> = VecDeque::new();
        let mut panics: Vec<BackendLogEntry> = Vec::new();
        for e in &ring.entries {
            in_buffer[e.level.index()] += 1;
            *targets.entry(e.target.as_str()).or_default() += 1;
            let key = (e.level, e.target.clone(), message_template(&e.message));
            let stat = templates.entry(key).or_insert_with(|| TemplateStat {
                count: 0,
                first_ts_ms: e.ts_ms,
                last_ts_ms: e.ts_ms,
                last_seq: e.seq,
                example: e.message.clone(),
            });
            stat.count += 1;
            stat.last_ts_ms = e.ts_ms;
            stat.last_seq = e.seq;
            if e.level >= LogLevel::Warn {
                recent_problems.push_back(e.clone());
                if recent_problems.len() > recent {
                    recent_problems.pop_front();
                }
            }
            if e.source == LogSource::Panic {
                panics.push(e.clone());
            }
        }
        let mut top_targets: Vec<(String, u64)> = targets
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        top_targets.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        top_targets.truncate(top);
        let mut repeated: Vec<RepeatedMessage> = templates
            .into_iter()
            .filter(|(_, s)| s.count > 1)
            .map(|((level, target, template), s)| RepeatedMessage {
                level,
                target,
                template,
                count: s.count,
                first_ts_ms: s.first_ts_ms,
                last_ts_ms: s.last_ts_ms,
                last_seq: s.last_seq,
                example: s.example,
            })
            .collect();
        repeated.sort_by(|a, b| {
            b.level
                .cmp(&a.level)
                .then(b.count.cmp(&a.count))
                .then_with(|| a.template.cmp(&b.template))
        });
        repeated.truncate(top);
        LogDigest {
            buffered: ring.entries.len(),
            next_seq: ring.next_seq,
            evicted: ring.evicted,
            oldest_ts_ms: ring.entries.front().map(|e| e.ts_ms),
            newest_ts_ms: ring.entries.back().map(|e| e.ts_ms),
            lifetime_counts: level_map(&ring.totals),
            buffered_counts: level_map(&in_buffer),
            top_targets: top_targets
                .into_iter()
                .map(|(target, count)| TargetCount { target, count })
                .collect(),
            repeated,
            recent_problems: recent_problems.into_iter().collect(),
            panics,
        }
    }

    /// Wait until an entry at/after `from_seq` satisfies `pred`, or `timeout`.
    /// Returns the first matching entry.
    pub async fn wait_for<F>(
        &self,
        from_seq: u64,
        timeout: std::time::Duration,
        pred: F,
    ) -> (Option<BackendLogEntry>, u64)
    where
        F: Fn(&BackendLogEntry) -> bool,
    {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut scanned_to = from_seq;
        // Entries evicted before this waiter could check them (a burst larger than the buffer
        // between two wake-ups): the wait cannot vouch for them, so the count is reported.
        let mut missed = 0;
        loop {
            // Register interest BEFORE scanning so an entry pushed between the
            // scan and the await still wakes us (no lost wake-up).
            let notified = self.notify.notified();
            tokio::pin!(notified);
            {
                let _scope = NotifyScope::enter();
                notified.as_mut().enable();
            }
            {
                let ring = self.lock();
                missed += ring.evicted_since(scanned_to);
                // Only the entries not checked yet: an index jump, not a rescan of the whole
                // ring under the lock the app's loggers need on every wake-up.
                for e in ring.entries.range(ring.index_of(scanned_to)..) {
                    if pred(e) {
                        return (Some(e.clone()), missed);
                    }
                }
                // Everything below next_seq has now been checked once.
                scanned_to = scanned_to.max(ring.next_seq);
            }
            if tokio::time::timeout_at(deadline, PollInNotifyScope(notified.as_mut()))
                .await
                .is_err()
            {
                return (None, missed);
            }
        }
    }

    /// Drop every buffered entry (the sequence counter keeps increasing).
    pub fn clear(&self) {
        let mut ring = self.lock();
        ring.entries.clear();
        ring.bytes = 0;
        ring.cleared_before = ring.next_seq;
    }
}

struct TemplateStat {
    count: u64,
    first_ts_ms: u64,
    last_ts_ms: u64,
    last_seq: u64,
    example: String,
}

fn level_map(counts: &[u64; 5]) -> BTreeMap<&'static str, u64> {
    [
        LogLevel::Trace,
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
    ]
    .into_iter()
    .map(|l| (l.as_str(), counts[l.index()]))
    .collect()
}

/// Collapse the variable parts of a message (numbers, hex, quoted strings,
/// UUID-ish tokens) so repeats of "the same line" group together.
#[must_use]
pub fn message_template(message: &str) -> String {
    let mut out = String::with_capacity(message.len().min(256));
    let mut chars = message.chars().peekable();
    let mut last_was_placeholder = false;
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            // Swallow the whole numeric/hex/uuid-ish run.
            while let Some(&n) = chars.peek() {
                if n.is_ascii_alphanumeric() || n == '.' || n == '-' || n == '_' || n == ':' {
                    chars.next();
                } else {
                    break;
                }
            }
            if !last_was_placeholder {
                out.push('#');
            }
            last_was_placeholder = true;
        } else if c == '"' {
            for n in chars.by_ref() {
                if n == '"' {
                    break;
                }
            }
            out.push_str("\"…\"");
            last_was_placeholder = true;
        } else {
            out.push(c);
            last_was_placeholder = false;
        }
        if out.len() >= 240 {
            out.push('…');
            break;
        }
    }
    out
}

// ── Queries ─────────────────────────────────────────────────────────────────

/// Filter + paging for [`LogBuffer::query`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LogQuery {
    /// Minimum level (inclusive).
    pub min_level: LogLevel,
    /// Only entries whose target starts with one of these prefixes (any, if empty).
    pub targets: Vec<String>,
    /// Case-insensitive substring matched against message, target, fields and spans.
    pub contains: Option<String>,
    /// Only entries with `seq >= since_seq`.
    pub since_seq: Option<u64>,
    /// Only entries captured at or after this Unix-epoch millisecond.
    pub since_ms: Option<u64>,
    /// Only entries whose structured fields match all of these (see [`field_matches`]).
    pub fields: Vec<(String, serde_json::Value)>,
    /// Maximum entries returned (the newest matches are kept).
    pub limit: usize,
}

impl Default for LogQuery {
    fn default() -> Self {
        Self {
            min_level: LogLevel::Trace,
            targets: Vec::new(),
            contains: None,
            since_seq: None,
            since_ms: None,
            fields: Vec::new(),
            limit: 100,
        }
    }
}

impl LogQuery {
    fn matcher(&self) -> Matcher<'_> {
        Matcher {
            q: self,
            needle: self.contains.as_ref().map(|s| s.to_lowercase()),
        }
    }
}

struct Matcher<'a> {
    q: &'a LogQuery,
    needle: Option<String>,
}

impl Matcher<'_> {
    fn matches(&self, e: &BackendLogEntry) -> bool {
        if e.level < self.q.min_level {
            return false;
        }
        if let Some(ms) = self.q.since_ms
            && e.ts_ms < ms
        {
            return false;
        }
        if !self.q.targets.is_empty()
            && !self
                .q
                .targets
                .iter()
                .any(|t| e.target.starts_with(t.as_str()))
        {
            return false;
        }
        if !entry_fields_match(e, &self.q.fields) {
            return false;
        }
        if let Some(needle) = &self.needle {
            return entry_contains(e, needle);
        }
        true
    }
}

/// `true` when the entry's structured field `key` equals `expected`.
///
/// Compared as text, case-insensitively, so it does not matter whether the app
/// logged a number (`elapsed_ms = 5`), a `Display` value (`%x`) or a string
/// (`run_type = "foreground_fast"`). This is what tells concurrent runs of the
/// same code path apart — a text match on the message alone cannot.
#[must_use]
pub fn field_matches(entry: &BackendLogEntry, key: &str, expected: &serde_json::Value) -> bool {
    fn text(v: &serde_json::Value) -> String {
        match v {
            serde_json::Value::String(s) => s.trim_matches('"').to_lowercase(),
            other => other.to_string().to_lowercase(),
        }
    }
    entry
        .fields
        .get(key)
        .is_some_and(|actual| text(actual) == text(expected))
}

/// `true` when every `(key, expected)` pair matches (vacuously true when empty).
#[must_use]
pub fn entry_fields_match(entry: &BackendLogEntry, fields: &[(String, serde_json::Value)]) -> bool {
    fields.iter().all(|(k, v)| field_matches(entry, k, v))
}

/// Case-insensitive substring search over every text part of an entry.
/// `needle` must already be lowercase.
#[must_use]
pub fn entry_contains(e: &BackendLogEntry, needle: &str) -> bool {
    if e.message.to_lowercase().contains(needle) || e.target.to_lowercase().contains(needle) {
        return true;
    }
    if e.spans.iter().any(|s| s.to_lowercase().contains(needle)) {
        return true;
    }
    e.fields.iter().any(|(k, v)| {
        k.to_lowercase().contains(needle)
            || match v {
                serde_json::Value::String(s) => s.to_lowercase().contains(needle),
                other => other.to_string().to_lowercase().contains(needle),
            }
    })
}

/// Result page of a [`LogBuffer::query`].
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct LogPage {
    /// Matching entries, oldest first.
    pub entries: Vec<BackendLogEntry>,
    /// How many buffered entries matched in total (may exceed `entries.len()`).
    pub total_matched: usize,
    /// `true` when `limit` cut the result (the newest matches were kept).
    pub truncated: bool,
    /// Pass back as `since_seq` to get only newer entries next time.
    pub next_seq: u64,
    /// Oldest sequence number still buffered.
    pub oldest_seq: u64,
    /// Entries evicted by the ring buffer since startup.
    pub evicted: u64,
    /// `true` when `since_seq` points before the oldest buffered entry —
    /// entries between the cursor and `oldest_seq` were evicted unseen.
    pub gap: bool,
}

/// Summary returned by [`LogBuffer::digest`].
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct LogDigest {
    /// Entries currently buffered.
    pub buffered: usize,
    /// Cursor for "everything after this digest".
    pub next_seq: u64,
    /// Entries evicted since startup.
    pub evicted: u64,
    /// Capture time of the oldest buffered entry.
    pub oldest_ts_ms: Option<u64>,
    /// Capture time of the newest buffered entry.
    pub newest_ts_ms: Option<u64>,
    /// Per-level totals since startup (including evicted entries).
    pub lifetime_counts: BTreeMap<&'static str, u64>,
    /// Per-level counts of what is still buffered.
    pub buffered_counts: BTreeMap<&'static str, u64>,
    /// Targets with the most buffered entries.
    pub top_targets: Vec<TargetCount>,
    /// Messages that repeat (numbers/strings collapsed), most severe then most frequent first.
    pub repeated: Vec<RepeatedMessage>,
    /// The most recent warnings and errors, oldest first.
    pub recent_problems: Vec<BackendLogEntry>,
    /// Every buffered panic.
    pub panics: Vec<BackendLogEntry>,
}

/// A target and how many buffered entries it produced.
#[derive(Debug, Clone, Serialize)]
pub struct TargetCount {
    /// Log target.
    pub target: String,
    /// Buffered entries from it.
    pub count: u64,
}

/// A repeating message template.
#[derive(Debug, Clone, Serialize)]
pub struct RepeatedMessage {
    /// Severity.
    pub level: LogLevel,
    /// Log target.
    pub target: String,
    /// Message with variable parts collapsed (`#` for numbers, `"…"` for strings).
    pub template: String,
    /// Occurrences in the buffer.
    pub count: u64,
    /// First occurrence (Unix ms).
    pub first_ts_ms: u64,
    /// Latest occurrence (Unix ms).
    pub last_ts_ms: u64,
    /// Sequence number of the latest occurrence.
    pub last_seq: u64,
    /// One concrete message.
    pub example: String,
}

// ── Global state ────────────────────────────────────────────────────────────

static GLOBAL: OnceLock<Arc<LogBuffer>> = OnceLock::new();
static SOURCES: AtomicU8 = AtomicU8::new(0);

const SRC_TRACING: u8 = 1;
const SRC_LOG: u8 = 1 << 1;
const SRC_PANIC: u8 = 1 << 2;

/// The process-wide capture buffer. Capacity comes from `VICTAURI_LOG_CAPACITY`
/// (entries) when set, else [`DEFAULT_CAPACITY`].
#[must_use]
pub fn global() -> Arc<LogBuffer> {
    GLOBAL
        .get_or_init(|| {
            let capacity = std::env::var("VICTAURI_LOG_CAPACITY")
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
                .filter(|n| (1..=100_000).contains(n))
                .unwrap_or(DEFAULT_CAPACITY);
            Arc::new(LogBuffer::new(
                capacity,
                DEFAULT_MAX_BYTES.max(capacity * 512),
            ))
        })
        .clone()
}

/// Which in-process capture sources are active in this process.
#[derive(Debug, Clone, Copy, Serialize)]
#[allow(clippy::struct_excessive_bools)]
#[non_exhaustive]
pub struct ActiveSources {
    /// [`log_layer`] is installed in the app's `tracing` subscriber and has seen an event.
    pub tracing: bool,
    /// [`log_logger`] / [`wrap_logger`] is installed.
    pub log: bool,
    /// The Victauri panic hook is installed.
    pub panic_hook: bool,
    /// The app runs under `victauri run` (raw stdout/stderr captured out-of-process).
    pub stdout_capture: bool,
}

impl ActiveSources {
    /// `true` when at least one structured source (tracing/log) is feeding the buffer.
    #[must_use]
    pub const fn any_structured(self) -> bool {
        self.tracing || self.log
    }
}

/// Snapshot of the active capture sources.
#[must_use]
pub fn active_sources() -> ActiveSources {
    let bits = SOURCES.load(Ordering::Relaxed);
    ActiveSources {
        tracing: bits & SRC_TRACING != 0,
        log: bits & SRC_LOG != 0,
        panic_hook: bits & SRC_PANIC != 0,
        stdout_capture: console_capture_path().is_some(),
    }
}

fn mark_source(bit: u8) {
    SOURCES.fetch_or(bit, Ordering::Relaxed);
}

/// `true` when capture must stay off: release builds, or `VICTAURI_DISABLE`.
fn capture_disabled() -> bool {
    if cfg!(not(debug_assertions)) {
        return true;
    }
    std::env::var("VICTAURI_DISABLE").is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// `true` for Victauri's own infrastructure (`victauri_*`, and the `rmcp` MCP SDK it
/// embeds). Those lines describe the introspection server, not the app — and every
/// agent request produces some, so capturing them would let *reading* the log
/// generate log noise. Set `VICTAURI_LOG_INTERNAL=1` to keep them.
#[must_use]
pub fn is_internal_target(target: &str) -> bool {
    static KEEP_INTERNAL: OnceLock<bool> = OnceLock::new();
    let keep = *KEEP_INTERNAL.get_or_init(|| {
        std::env::var("VICTAURI_LOG_INTERNAL").is_ok_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
    });
    if keep {
        return false;
    }
    target.starts_with("victauri_")
        || target == "victauri"
        || target == "rmcp"
        || target.starts_with("rmcp::")
}

/// Tokio's own runtime instrumentation (`tokio_unstable` + its `tracing` feature: waker and
/// task events for tokio-console). Always excluded, whatever `VICTAURI_LOG_INTERNAL` says: some
/// of it is emitted while tokio holds internal locks, so capturing it is never safe, and it is
/// runtime plumbing, not the app's log.
#[must_use]
pub fn is_runtime_internal_target(target: &str) -> bool {
    target == "tokio"
        || target.starts_with("tokio::")
        || target.starts_with("runtime::")
        || target.starts_with("runtime.")
}

/// The least severe level captured: `VICTAURI_LOG_LEVEL` (trace/debug/info/warn/error), default
/// `debug` — trace-level output from every dependency would mostly be noise that evicts the
/// app's own lines.
#[must_use]
pub fn capture_level() -> LogLevel {
    static LEVEL: OnceLock<LogLevel> = OnceLock::new();
    *LEVEL.get_or_init(|| {
        std::env::var("VICTAURI_LOG_LEVEL")
            .ok()
            .and_then(|v| LogLevel::parse(&v))
            .unwrap_or(LogLevel::Debug)
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn truncate_string(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut cut = max;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        let extra = s.len() - cut;
        s.truncate(cut);
        let _ = write!(s, "…[+{extra} bytes]");
    }
    s
}

fn current_thread_name() -> Option<String> {
    std::thread::current().name().map(str::to_string)
}

// ── tracing layer ───────────────────────────────────────────────────────────

/// `tracing_subscriber` layer that copies every event into Victauri's backend
/// log buffer. Obtain it with [`log_layer`].
#[derive(Clone)]
#[cfg(debug_assertions)]
pub struct BackendLogLayer {
    buffer: Arc<LogBuffer>,
}

#[cfg(debug_assertions)]
impl std::fmt::Debug for BackendLogLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendLogLayer").finish_non_exhaustive()
    }
}

#[cfg(debug_assertions)]
impl BackendLogLayer {
    /// A layer writing into a specific buffer (tests); apps use [`log_layer`].
    #[must_use]
    pub const fn with_buffer(buffer: Arc<LogBuffer>) -> Self {
        Self { buffer }
    }
}

/// Release builds: a zero-sized layer with an empty [`Layer`] impl. [`log_layer`] returns
/// `None` in release anyway, but the app monomorphises the layer type's methods into its own
/// binary whatever the value — so the type itself must be empty for the documented one-line
/// recipe to leave no Victauri code in a release build.
#[cfg(not(debug_assertions))]
#[derive(Debug, Clone, Copy)]
pub struct BackendLogLayer {
    _private: (),
}

#[cfg(not(debug_assertions))]
impl BackendLogLayer {
    /// A layer writing into a specific buffer (tests); apps use [`log_layer`].
    #[must_use]
    pub fn with_buffer(_buffer: Arc<LogBuffer>) -> Self {
        Self { _private: () }
    }
}

#[cfg(not(debug_assertions))]
impl<S: Subscriber> Layer<S> for BackendLogLayer {}

/// The `tracing` layer that feeds Victauri's backend log capture.
///
/// Add it to the app's subscriber **where the app already builds it** — usually
/// the first lines of `main`, before `tauri::Builder`:
///
/// ```ignore
/// use tracing_subscriber::prelude::*;
///
/// tracing_subscriber::registry()
///     .with(tracing_subscriber::EnvFilter::new("info"))
///     .with(tracing_subscriber::fmt::layer())
///     .with(victauri_plugin::log_layer()) // ← the one added line
///     .init();
/// ```
///
/// It sees what passes the subscriber's *global* filter (an `EnvFilter` added
/// with `.with(...)` filters every layer). Returns `None` — a no-op layer — in
/// release builds and when `VICTAURI_DISABLE` is set, and in release builds the
/// layer type itself is empty, so the line adds no Victauri code to a release
/// binary and can stay in the app unconditionally — unless Victauri is behind a
/// Cargo feature (`victauri init --optional`): then gate the line on that feature
/// too, since it names this crate. A later `set_global_default` cannot add a layer to a
/// subscriber the app already installed, which is why this is an explicit line
/// rather than something `init()` could do on its own.
#[must_use]
pub fn log_layer() -> Option<BackendLogLayer> {
    if capture_disabled() {
        return None;
    }
    Some(BackendLogLayer::with_buffer(global()))
}

/// Span fields recorded once per span and reused for every event inside it.
#[cfg(debug_assertions)]
struct SpanFields(String);

#[cfg(debug_assertions)]
impl<S> Layer<S> for BackendLogLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    // An upper bound, not a filter: this layer never disables anything for other layers (a
    // `Layer::enabled` returning false would). Without a hint, an app whose own layers all use
    // per-layer filters ends up with a global max level of TRACE, so every trace-level callsite
    // in every dependency would be built for this layer alone.
    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(capture_level().to_level_filter())
    }

    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let mut v = SpanVisitor::default();
        attrs.record(&mut v);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanFields(v.0));
        }
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        if let Some(span) = ctx.span(id) {
            let mut ext = span.extensions_mut();
            let mut v = SpanVisitor(
                ext.get_mut::<SpanFields>()
                    .map(|f| std::mem::take(&mut f.0))
                    .unwrap_or_default(),
            );
            values.record(&mut v);
            if let Some(f) = ext.get_mut::<SpanFields>() {
                f.0 = v.0;
            } else {
                ext.insert(SpanFields(v.0));
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        mark_source(SRC_TRACING);
        let meta = event.metadata();
        if is_internal_target(meta.target())
            || is_runtime_internal_target(meta.target())
            || LogLevel::from_tracing(*meta.level()) < capture_level()
        {
            return;
        }
        let mut v = EventVisitor::default();
        event.record(&mut v);

        // `tracing-log` bridges `log` records as events whose real target and
        // location travel in `log.*` fields — surface those instead.
        let mut target = meta.target().to_string();
        let mut location = meta
            .file()
            .map(|f| format!("{f}:{}", meta.line().unwrap_or(0)));
        if let Some(serde_json::Value::String(t)) = v.fields.remove("log.target") {
            target = t;
        }
        let log_file = v.fields.remove("log.file");
        let log_line = v.fields.remove("log.line");
        v.fields.remove("log.module_path");
        if let Some(serde_json::Value::String(f)) = log_file {
            location = Some(format!(
                "{f}:{}",
                log_line.as_ref().map_or(0, |l| l.as_u64().unwrap_or(0))
            ));
        }

        let spans = ctx
            .event_scope(event)
            .map(|scope| {
                scope
                    .from_root()
                    .map(|span| {
                        let ext = span.extensions();
                        match ext.get::<SpanFields>() {
                            Some(f) if !f.0.is_empty() => format!("{}{{{}}}", span.name(), f.0),
                            _ => span.name().to_string(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        self.buffer.push(BackendLogEntry {
            seq: 0,
            ts_ms: now_ms(),
            level: LogLevel::from_tracing(*meta.level()),
            target,
            message: truncate_string(v.message, MAX_MESSAGE_BYTES),
            fields: v.fields,
            spans,
            thread: current_thread_name(),
            location,
            source: LogSource::Tracing,
        });
    }
}

#[derive(Default)]
struct EventVisitor {
    message: String,
    fields: serde_json::Map<String, serde_json::Value>,
}

impl EventVisitor {
    fn put(&mut self, field: &Field, value: serde_json::Value) {
        if self.fields.len() < 64 {
            self.fields.insert(field.name().to_string(), value);
        }
    }
}

impl Visit for EventVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = debug_bounded(value, MAX_MESSAGE_BYTES);
        } else {
            self.put(
                field,
                serde_json::Value::String(debug_bounded(value, MAX_FIELD_BYTES)),
            );
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            value.clone_into(&mut self.message);
        } else {
            self.put(
                field,
                serde_json::Value::String(truncate_string(value.to_string(), MAX_FIELD_BYTES)),
            );
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, value.into());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, value.into());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, value.into());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, value.into());
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        let mut text = value.to_string();
        let mut source = value.source();
        let mut depth = 0;
        while let Some(s) = source {
            if depth >= 8 {
                break;
            }
            let _ = write!(text, ": {s}");
            source = s.source();
            depth += 1;
        }
        self.put(
            field,
            serde_json::Value::String(truncate_string(text, MAX_FIELD_BYTES)),
        );
    }
}

#[derive(Default)]
struct SpanVisitor(String);

impl Visit for SpanVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if self.0.len() > 512 {
            return;
        }
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        // Each value is bounded like an event field: the span string is cloned into every
        // event inside the span, so an unbounded `#[instrument]` argument (a 10 MB buffer)
        // used to cost its whole Debug size per event and flush the ring.
        let value = debug_bounded(value, MAX_FIELD_BYTES);
        if field.name() == "message" {
            self.0.push_str(&value);
        } else {
            let _ = write!(self.0, "{}={value}", field.name());
        }
    }
}

// ── log crate adapter ───────────────────────────────────────────────────────

/// A [`log::Log`] that copies records into Victauri's capture and (optionally)
/// forwards them to the app's own logger.
pub struct VictauriLogger {
    inner: Option<Box<dyn log::Log>>,
    buffer: Arc<LogBuffer>,
}

impl std::fmt::Debug for VictauriLogger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VictauriLogger")
            .field("forwards", &self.inner.is_some())
            .finish_non_exhaustive()
    }
}

impl VictauriLogger {
    /// A logger writing into a specific buffer (tests).
    #[must_use]
    pub fn with_buffer(buffer: Arc<LogBuffer>, inner: Option<Box<dyn log::Log>>) -> Self {
        Self { inner, buffer }
    }
}

impl log::Log for VictauriLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        // `enabled` only answers `log_enabled!` (the `log!` macros call `log` directly, which
        // captures regardless): with a wrapped logger, answer exactly as it does, so app code
        // guarded by `log_enabled!` keeps doing what it did before Victauri was added.
        match &self.inner {
            Some(inner) => inner.enabled(metadata),
            None => metadata.level() <= log::max_level(),
        }
    }

    fn log(&self, record: &log::Record<'_>) {
        mark_source(SRC_LOG);
        if is_internal_target(record.target())
            || is_runtime_internal_target(record.target())
            || LogLevel::from_log(record.level()) < capture_level()
        {
            if let Some(inner) = &self.inner
                && inner.enabled(record.metadata())
            {
                inner.log(record);
            }
            return;
        }
        let mut fields = serde_json::Map::new();
        if let Some(mp) = record.module_path()
            && mp != record.target()
        {
            fields.insert("module_path".into(), mp.into());
        }
        self.buffer.push(BackendLogEntry {
            seq: 0,
            ts_ms: now_ms(),
            level: LogLevel::from_log(record.level()),
            target: record.target().to_string(),
            message: truncate_string(record.args().to_string(), MAX_MESSAGE_BYTES),
            fields,
            spans: Vec::new(),
            thread: current_thread_name(),
            location: record
                .file()
                .map(|f| format!("{f}:{}", record.line().unwrap_or(0))),
            source: LogSource::Log,
        });
        if let Some(inner) = &self.inner
            && inner.enabled(record.metadata())
        {
            inner.log(record);
        }
    }

    fn flush(&self) {
        if let Some(inner) = &self.inner {
            inner.flush();
        }
    }
}

/// Capture-only [`log::Log`] sink, for logger frameworks that fan out to
/// several outputs — e.g. `fern::Dispatch::new().chain(victauri_plugin::log_logger())`,
/// or a `tauri-plugin-log` dispatch target. In release builds (or with
/// `VICTAURI_DISABLE`) the sink discards everything.
#[must_use]
pub fn log_logger() -> Box<dyn log::Log> {
    if capture_disabled() {
        return Box::new(NullLogger);
    }
    Box::new(VictauriLogger::with_buffer(global(), None))
}

/// Wrap the app's own [`log::Log`] so every record is captured **and** still
/// printed by it. For `env_logger`-style setups:
///
/// ```ignore
/// let inner = env_logger::Builder::from_default_env().build();
/// let max = inner.filter();
/// log::set_boxed_logger(victauri_plugin::wrap_logger(Box::new(inner))).ok();
/// log::set_max_level(max);
/// ```
///
/// In release builds (or with `VICTAURI_DISABLE`) the inner logger is returned
/// unchanged.
#[must_use]
pub fn wrap_logger(inner: Box<dyn log::Log>) -> Box<dyn log::Log> {
    if capture_disabled() {
        return inner;
    }
    Box::new(VictauriLogger::with_buffer(global(), Some(inner)))
}

struct NullLogger;

impl log::Log for NullLogger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        false
    }
    fn log(&self, _: &log::Record<'_>) {}
    fn flush(&self) {}
}

// ── panic hook ──────────────────────────────────────────────────────────────

/// Install the panic-capture hook (idempotent). Chains to whatever hook was
/// installed before, so the app's own hook (and the default stderr message)
/// still run. Called by the plugin in debug builds — apps don't call this.
pub fn install_panic_hook() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if capture_disabled() {
        return;
    }
    INSTALLED.get_or_init(|| {
        let buffer = global();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // The app's own hook first: it may be a security hook (4DA's crash guard zeroises
            // secrets and writes its crash log) that must not wait on this one.
            previous(info);
            capture_panic(&buffer, info);
        }));
        mark_source(SRC_PANIC);
    });
}

/// A panic's backtrace, captured on the panicking thread but symbolised only when an agent
/// reads the entry: symbol resolution is the expensive part (the first one on Windows loads
/// debug info — measured over 10 s in a large app), and it must not run on the panicking
/// thread, which may be the UI thread or a panic the app catches on purpose.
struct PendingBacktrace {
    backtrace: std::backtrace::Backtrace,
    resolved: OnceLock<String>,
}

fn pending_backtraces() -> &'static Mutex<HashMap<u32, Arc<PendingBacktrace>>> {
    static PENDING: OnceLock<Mutex<HashMap<u32, Arc<PendingBacktrace>>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Replace every `backtrace_id` placeholder in `value` (an entry, or any JSON holding entries)
/// with the symbolised, trimmed backtrace. Runs on the reading side — the server's runtime —
/// and resolves each backtrace once.
pub fn resolve_backtraces(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(id) = map
                .get("backtrace_id")
                .and_then(serde_json::Value::as_u64)
                .and_then(|id| u32::try_from(id).ok())
            {
                let slot = pending_backtraces()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&id)
                    .cloned();
                if let Some(slot) = slot {
                    let text = slot.resolved.get_or_init(|| {
                        truncate_string(
                            trim_backtrace(&slot.backtrace.to_string()),
                            MAX_BACKTRACE_BYTES,
                        )
                    });
                    map.insert("backtrace".into(), text.clone().into());
                }
                map.remove("backtrace_id");
            }
            for v in map.values_mut() {
                resolve_backtraces(v);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(resolve_backtraces),
        _ => {}
    }
}

fn capture_panic(buffer: &LogBuffer, info: &std::panic::PanicHookInfo<'_>) {
    // Capturing the frames is cheap; symbolising them is not (see `PendingBacktrace`), so that
    // is deferred to the reader. Only the first few panics keep a backtrace at all.
    static BACKTRACES_TAKEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_string());
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
    let mut fields = serde_json::Map::new();
    let n = BACKTRACES_TAKEN.fetch_add(1, Ordering::Relaxed);
    if n < MAX_PANIC_BACKTRACES {
        let slot = Arc::new(PendingBacktrace {
            backtrace: std::backtrace::Backtrace::force_capture(),
            resolved: OnceLock::new(),
        });
        // try_lock: a panic while this thread holds the map (it never does any real work
        // under it) must not deadlock the hook.
        if let Ok(mut pending) = pending_backtraces().try_lock() {
            pending.insert(n, slot);
            fields.insert("backtrace_id".into(), n.into());
            fields.insert("backtrace".into(), "(symbolised when read)".into());
        }
    } else {
        fields.insert(
            "backtrace".into(),
            format!("omitted — only the first {MAX_PANIC_BACKTRACES} panics carry a backtrace")
                .into(),
        );
    }
    let thread = std::thread::current()
        .name()
        .map_or_else(|| "<unnamed>".to_string(), str::to_string);
    let _ = buffer.try_push(BackendLogEntry {
        seq: 0,
        ts_ms: now_ms(),
        level: LogLevel::Error,
        target: "panic".to_string(),
        message: truncate_string(message, MAX_MESSAGE_BYTES),
        fields,
        spans: Vec::new(),
        thread: Some(thread),
        location,
        source: LogSource::Panic,
    });
}

/// How many panics get a (resolved) backtrace before the hook stops paying for them.
pub const MAX_PANIC_BACKTRACES: u32 = 20;
/// Frames kept per panic backtrace, after the panic machinery is trimmed.
pub const MAX_BACKTRACE_FRAMES: usize = 40;

/// Drop the leading frames that belong to the backtrace/panic machinery and this
/// hook, so frame 0 of what an agent reads is the code that panicked; keep at
/// most [`MAX_BACKTRACE_FRAMES`] frames.
#[must_use]
pub fn trim_backtrace(raw: &str) -> String {
    const MACHINERY: [&str; 9] = [
        "std::backtrace",
        "std::sys::backtrace",
        "backtrace_rs",
        "victauri_plugin::backend_logs",
        "std::panicking",
        "core::panicking",
        "rust_begin_unwind",
        "alloc::boxed::",
        "__rustc",
    ];
    // Group lines into frames: a frame starts with "<spaces><n>: symbol".
    let mut frames: Vec<Vec<&str>> = Vec::new();
    for line in raw.lines() {
        let t = line.trim_start();
        let is_header = t
            .split_once(':')
            .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if is_header || frames.is_empty() {
            frames.push(vec![line]);
        } else if let Some(last) = frames.last_mut() {
            last.push(line);
        }
    }
    let first_real = frames
        .iter()
        .position(|f| !MACHINERY.iter().any(|m| f[0].contains(m)))
        .unwrap_or(0);
    let kept: Vec<String> = frames
        .iter()
        .skip(first_real)
        .take(MAX_BACKTRACE_FRAMES)
        .map(|f| f.join("\n"))
        .collect();
    let dropped = frames.len().saturating_sub(first_real + kept.len());
    let mut out = kept.join("\n");
    if dropped > 0 {
        let _ = write!(out, "\n   … {dropped} more frames");
    }
    out
}

// ── console capture (written by `victauri run`) ─────────────────────────────

/// Environment variable `victauri run` sets on the app: the JSONL file it writes
/// the app's stdout/stderr lines to.
pub const CONSOLE_CAPTURE_ENV: &str = "VICTAURI_CONSOLE_LOG";

/// Path of the out-of-process console capture, when the app runs under `victauri run`.
#[must_use]
pub fn console_capture_path() -> Option<std::path::PathBuf> {
    std::env::var_os(CONSOLE_CAPTURE_ENV)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
}

/// One line captured by `victauri run`.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ConsoleLine {
    /// Byte offset of the record in the capture file (use as a cursor).
    #[serde(default)]
    pub offset: u64,
    /// Unix-epoch milliseconds when the launcher read the line.
    #[serde(rename = "t")]
    pub ts_ms: u64,
    /// `out`, `err`, or `exit` (the launcher's final record: how the app ended).
    #[serde(rename = "s")]
    pub stream: String,
    /// The line, ANSI escapes removed.
    #[serde(rename = "l")]
    pub text: String,
    /// Level inferred from common log formats (`INFO`, `[WARN]`, `error:` …), when recognisable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<LogLevel>,
    /// On the `exit` record only: the app's last stderr lines (its last words).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tail: Vec<String>,
}

/// Read console-capture records starting at byte `from` (or the last
/// `tail_bytes` of the file when `from` is `None`). Returns the records and the
/// offset to resume from.
///
/// # Errors
/// Returns an error when the file cannot be opened or read.
pub fn read_console_capture(
    path: &std::path::Path,
    from: Option<u64>,
    tail_bytes: u64,
) -> std::io::Result<(Vec<ConsoleLine>, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut start = from.unwrap_or_else(|| len.saturating_sub(tail_bytes));
    if start > len {
        // File was rotated/truncated under us: start over.
        start = 0;
    }
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.take(16 * 1024 * 1024).read_to_end(&mut buf)?;
    let mut lines = Vec::new();
    let mut offset = start;
    let mut consumed = 0usize;
    let mut first = from.is_none() && start > 0;
    for chunk in buf.split_inclusive(|b| *b == b'\n') {
        if !chunk.ends_with(b"\n") {
            break; // partial record still being written
        }
        consumed += chunk.len();
        let rec_offset = offset;
        offset += chunk.len() as u64;
        if first {
            // A tail read may start mid-record — skip to the first full line.
            first = false;
            continue;
        }
        if let Ok(mut line) = serde_json::from_slice::<ConsoleLine>(chunk) {
            line.offset = rec_offset;
            if line.level.is_none() {
                line.level = infer_level(&line.text);
            }
            lines.push(line);
        }
    }
    Ok((lines, start + consumed as u64))
}

/// Best-effort level detection for a plain console line (`tracing`'s
/// `  INFO target:`, `env_logger`'s `[... WARN target]`, `error:` / `panicked at`).
#[must_use]
pub fn infer_level(text: &str) -> Option<LogLevel> {
    if text.contains("panicked at") || text.starts_with("error:") || text.starts_with("error[") {
        return Some(LogLevel::Error);
    }
    if text.starts_with("warning:") {
        return Some(LogLevel::Warn);
    }
    // Look at the first few whitespace/bracket separated tokens only, so a
    // message that merely mentions "error" is not misclassified.
    for token in text
        .split(|c: char| c.is_whitespace() || c == '[' || c == ']')
        .filter(|t| !t.is_empty())
        .take(4)
    {
        match token {
            "TRACE" => return Some(LogLevel::Trace),
            "DEBUG" => return Some(LogLevel::Debug),
            "INFO" => return Some(LogLevel::Info),
            "WARN" | "WARNING" => return Some(LogLevel::Warn),
            "ERROR" | "FATAL" => return Some(LogLevel::Error),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
#[path = "backend_logs_tests.rs"]
mod tests;
