//! Backend introspection and chaos engineering types.
//!
//! These types support Victauri's intervention capabilities — features that exploit
//! the plugin's position inside the Rust process to provide insights and control
//! that browser-external tools like CDP cannot access.

use std::collections::{HashMap, VecDeque};
use std::sync::RwLock;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use serde::Serialize;

/// Per-command timing statistics aggregated from IPC invocations.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct CommandTimingStats {
    /// Command name.
    pub command: String,
    /// Number of invocations recorded.
    pub count: u64,
    /// Minimum execution time in milliseconds.
    pub min_ms: f64,
    /// Maximum execution time in milliseconds.
    pub max_ms: f64,
    /// Mean execution time in milliseconds.
    pub avg_ms: f64,
    /// 95th percentile execution time in milliseconds.
    pub p95_ms: f64,
    /// Total execution time across all invocations.
    pub total_ms: f64,
}

/// Maximum recent samples retained per command for percentile estimation.
/// `count`, `total`, `min`, and `max` are tracked as running aggregates, so they
/// stay accurate over the command's full history; only the p95 estimate is
/// windowed to this many of the most recent samples. This bounds memory under a
/// long agent soak that hammers `invoke_command` (the prior `Vec` grew forever
/// and was re-sorted in full on every `command_timings` read).
const MAX_TIMING_SAMPLES: usize = 1024;

/// Accumulated timing data for a single command — bounded memory.
///
/// Internal accumulator behind `CommandTimings` (crate-private — never part of the public
/// API, so its layout can evolve freely without a semver break).
#[derive(Debug, Default)]
pub(crate) struct TimingSamples {
    /// Most-recent durations (ring, capped at `MAX_TIMING_SAMPLES`) for p95.
    recent: VecDeque<Duration>,
    /// Total invocations recorded (all time).
    count: u64,
    /// Sum of all durations (all time) — for accurate mean/total.
    total: Duration,
    /// All-time minimum duration.
    min: Option<Duration>,
    /// All-time maximum duration.
    max: Option<Duration>,
}

impl TimingSamples {
    /// Add a timing sample.
    pub fn record(&mut self, duration: Duration) {
        self.count += 1;
        self.total = self.total.saturating_add(duration);
        self.min = Some(self.min.map_or(duration, |m| m.min(duration)));
        self.max = Some(self.max.map_or(duration, |m| m.max(duration)));
        if self.recent.len() == MAX_TIMING_SAMPLES {
            self.recent.pop_front();
        }
        self.recent.push_back(duration);
    }

    /// Compute aggregate statistics. `count`, `min`, `max`, `avg`, and `total`
    /// reflect the full history; `p95` is estimated over the most recent
    /// `MAX_TIMING_SAMPLES` samples.
    #[must_use]
    pub fn stats(&self, command: &str) -> CommandTimingStats {
        if self.count == 0 {
            return CommandTimingStats {
                command: command.to_string(),
                count: 0,
                min_ms: 0.0,
                max_ms: 0.0,
                avg_ms: 0.0,
                p95_ms: 0.0,
                total_ms: 0.0,
            };
        }
        let to_ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let round2 = |v: f64| (v * 100.0).round() / 100.0;

        let total_ms = to_ms(self.total);
        let avg_ms = total_ms / self.count as f64;
        let min_ms = self.min.map_or(0.0, to_ms);
        let max_ms = self.max.map_or(0.0, to_ms);

        // p95 over the recent window (bounded; representative of current behavior).
        let mut sorted: Vec<f64> = self.recent.iter().copied().map(to_ms).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p95 = if sorted.is_empty() {
            0.0
        } else {
            // Nearest-rank: the ceil(0.95·n)-th value, i.e. 0-based index ceil(0.95·n) − 1.
            // Without the −1 this returned the MAX for every n ≤ 20.
            let idx = (((sorted.len() as f64) * 0.95).ceil() as usize).saturating_sub(1);
            sorted[idx.min(sorted.len() - 1)]
        };

        CommandTimingStats {
            command: command.to_string(),
            count: self.count,
            min_ms: round2(min_ms),
            max_ms: round2(max_ms),
            avg_ms: round2(avg_ms),
            p95_ms: round2(p95),
            total_ms: round2(total_ms),
        }
    }
}

/// Maximum distinct commands tracked. The map is keyed by the caller-supplied command name, so
/// without a cap an agent (or a loop) invoking ever-new names grows it forever. Once full, new
/// names are not tracked; commands already tracked keep accumulating.
const MAX_TIMED_COMMANDS: usize = 1024;

/// Thread-safe store for per-command timing data.
pub struct CommandTimings {
    inner: RwLock<HashMap<String, TimingSamples>>,
}

impl CommandTimings {
    /// Create a new empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Record a timing sample for a command.
    pub fn record(&self, command: &str, duration: Duration) {
        let mut map = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(samples) = map.get_mut(command) {
            samples.record(duration);
        } else if map.len() < MAX_TIMED_COMMANDS {
            map.entry(command.to_string()).or_default().record(duration);
        }
    }

    /// Get stats for all commands, sorted by total time descending.
    #[must_use]
    pub fn all_stats(&self) -> Vec<CommandTimingStats> {
        let map = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut stats: Vec<CommandTimingStats> =
            map.iter().map(|(name, s)| s.stats(name)).collect();
        stats.sort_by(|a, b| {
            b.total_ms
                .partial_cmp(&a.total_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        stats
    }

    /// Get stats for a single command.
    #[must_use]
    pub fn stats_for(&self, command: &str) -> Option<CommandTimingStats> {
        let map = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.get(command).map(|s| s.stats(command))
    }

    /// Clear all timing data.
    pub fn clear(&self) {
        let mut map = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.clear();
    }
}

impl Default for CommandTimings {
    fn default() -> Self {
        Self::new()
    }
}

// ── Fault Injection ─────────────────────────────────────────────────────────

/// The type of fault to inject into a command.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub enum FaultType {
    /// Add artificial latency before command execution.
    Delay {
        /// Delay in milliseconds.
        delay_ms: u64,
    },
    /// Return an error without executing the command.
    Error {
        /// Error message to return.
        message: String,
    },
    /// Drop the response entirely (return empty/timeout-like response).
    Drop,
    /// Execute normally but corrupt the response (randomize field values).
    Corrupt,
}

/// Configuration for a single fault injection rule.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct FaultConfig {
    /// Target command name.
    pub command: String,
    /// Type of fault to inject.
    pub fault_type: FaultType,
    /// Number of times this fault has been triggered.
    pub trigger_count: u64,
    /// Maximum number of times to trigger (0 = unlimited).
    pub max_triggers: u64,
    /// When this fault was created.
    #[serde(skip)]
    pub created_at: Instant,
}

/// Faults auto-expire this long after creation, so a forgotten fault cannot
/// silently sabotage or mask a later test run (audit #34). Re-inject to refresh.
pub const FAULT_TTL: Duration = Duration::from_secs(900); // 15 minutes

impl FaultConfig {
    /// Creates a fault rule for `command`, created now with zero triggers so far.
    /// `max_triggers == 0` means unlimited (until [`FAULT_TTL`] expires it).
    #[must_use]
    pub fn new(command: impl Into<String>, fault_type: FaultType, max_triggers: u64) -> Self {
        Self {
            command: command.into(),
            fault_type,
            trigger_count: 0,
            max_triggers,
            created_at: Instant::now(),
        }
    }

    /// Whether this fault should still trigger, evaluated at `now`. A fault is
    /// inert once it is older than [`FAULT_TTL`] or has hit `max_triggers`.
    #[must_use]
    pub fn should_trigger_at(&self, now: Instant) -> bool {
        if now.saturating_duration_since(self.created_at) >= FAULT_TTL {
            return false;
        }
        self.max_triggers == 0 || self.trigger_count < self.max_triggers
    }

    /// Check if this fault should still trigger right now.
    #[must_use]
    pub fn should_trigger(&self) -> bool {
        self.should_trigger_at(Instant::now())
    }
}

/// Thread-safe registry of active fault injection rules.
pub struct FaultRegistry {
    inner: RwLock<HashMap<String, FaultConfig>>,
}

impl FaultRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Register a fault for a command.
    pub fn inject(&self, config: FaultConfig) {
        let mut map = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.insert(config.command.clone(), config);
    }

    /// Look up and optionally trigger a fault for a command.
    /// Returns the fault type if one is active and should trigger.
    pub fn check_and_trigger(&self, command: &str) -> Option<FaultType> {
        let mut map = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(config) = map.get_mut(command)
            && config.should_trigger()
        {
            config.trigger_count += 1;
            return Some(config.fault_type.clone());
        }
        None
    }

    /// List all active fault rules.
    #[must_use]
    pub fn list(&self) -> Vec<FaultConfig> {
        let map = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.values().cloned().collect()
    }

    /// Remove a fault rule for a command.
    pub fn clear(&self, command: &str) -> bool {
        let mut map = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.remove(command).is_some()
    }

    /// Remove all fault rules.
    pub fn clear_all(&self) -> usize {
        let mut map = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = map.len();
        map.clear();
        count
    }
}

impl Default for FaultRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── IPC Contract Testing ────────────────────────────────────────────────────

/// Describes the shape of a JSON value for contract comparison.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[non_exhaustive]
pub enum JsonShape {
    /// null
    Null,
    /// boolean
    Bool,
    /// number (integer or float)
    Number,
    /// string
    String,
    /// array with element shape (from first element, or Null if empty)
    Array(Box<Self>),
    /// object with field names and their shapes
    Object(HashMap<String, Self>),
}

impl JsonShape {
    /// Extract the shape of a JSON value.
    #[must_use]
    pub fn from_value(value: &serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(_) => Self::Bool,
            serde_json::Value::Number(_) => Self::Number,
            serde_json::Value::String(_) => Self::String,
            serde_json::Value::Array(arr) => {
                let elem = arr.first().map_or(Self::Null, Self::from_value);
                Self::Array(Box::new(elem))
            }
            serde_json::Value::Object(obj) => {
                let fields: HashMap<String, Self> = obj
                    .iter()
                    .map(|(k, v)| (k.clone(), Self::from_value(v)))
                    .collect();
                Self::Object(fields)
            }
        }
    }

    /// Human-readable type name.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool => "bool",
            Self::Number => "number",
            Self::String => "string",
            Self::Array(_) => "array",
            Self::Object(_) => "object",
        }
    }
}

/// A recorded contract baseline for a command's response.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ContractBaseline {
    /// Command name.
    pub command: String,
    /// Arguments used when recording.
    pub args: serde_json::Value,
    /// Shape of the response.
    pub shape: JsonShape,
    /// Raw sample response (first 4KB).
    pub sample: String,
    /// When this baseline was recorded.
    pub recorded_at: String,
}

impl ContractBaseline {
    /// Creates a contract baseline for `command`.
    #[must_use]
    pub fn new(
        command: impl Into<String>,
        args: serde_json::Value,
        shape: JsonShape,
        sample: impl Into<String>,
        recorded_at: impl Into<String>,
    ) -> Self {
        Self {
            command: command.into(),
            args,
            shape,
            sample: sample.into(),
            recorded_at: recorded_at.into(),
        }
    }
}

/// Differences found when checking a contract against baseline.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ContractDrift {
    /// Command name.
    pub command: String,
    /// Fields present in current but not in baseline.
    pub new_fields: Vec<String>,
    /// Fields present in baseline but not in current.
    pub removed_fields: Vec<String>,
    /// Fields whose type changed.
    pub type_changes: Vec<TypeChange>,
    /// Whether the overall shape matches.
    pub shape_matches: bool,
}

/// A single field type change between baseline and current.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct TypeChange {
    /// Dot-separated field path.
    pub path: String,
    /// Type in the baseline.
    pub baseline_type: String,
    /// Type in the current response.
    pub current_type: String,
}

/// Compare two JSON shapes and report differences.
#[must_use]
pub fn diff_shapes(baseline: &JsonShape, current: &JsonShape, prefix: &str) -> ContractDrift {
    let mut new_fields = Vec::new();
    let mut removed_fields = Vec::new();
    let mut type_changes = Vec::new();

    diff_shapes_inner(
        baseline,
        current,
        prefix,
        &mut new_fields,
        &mut removed_fields,
        &mut type_changes,
    );

    let shape_matches =
        new_fields.is_empty() && removed_fields.is_empty() && type_changes.is_empty();
    ContractDrift {
        command: prefix.to_string(),
        new_fields,
        removed_fields,
        type_changes,
        shape_matches,
    }
}

fn diff_shapes_inner(
    baseline: &JsonShape,
    current: &JsonShape,
    prefix: &str,
    new_fields: &mut Vec<String>,
    removed_fields: &mut Vec<String>,
    type_changes: &mut Vec<TypeChange>,
) {
    match (baseline, current) {
        (JsonShape::Object(b_fields), JsonShape::Object(c_fields)) => {
            for (key, b_shape) in b_fields {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                if let Some(c_shape) = c_fields.get(key) {
                    diff_shapes_inner(
                        b_shape,
                        c_shape,
                        &path,
                        new_fields,
                        removed_fields,
                        type_changes,
                    );
                } else {
                    removed_fields.push(path);
                }
            }
            for key in c_fields.keys() {
                if !b_fields.contains_key(key) {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    new_fields.push(path);
                }
            }
        }
        (JsonShape::Array(b_elem), JsonShape::Array(c_elem)) => {
            let path = format!("{prefix}[]");
            diff_shapes_inner(
                b_elem,
                c_elem,
                &path,
                new_fields,
                removed_fields,
                type_changes,
            );
        }
        (b, c) if b.type_name() != c.type_name() => {
            type_changes.push(TypeChange {
                path: prefix.to_string(),
                baseline_type: b.type_name().to_string(),
                current_type: c.type_name().to_string(),
            });
        }
        _ => {}
    }
}

/// Thread-safe store for IPC contract baselines.
pub struct ContractStore {
    inner: RwLock<HashMap<String, ContractBaseline>>,
}

impl ContractStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Record a baseline for a command.
    pub fn record(&self, baseline: ContractBaseline) {
        let mut map = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.insert(baseline.command.clone(), baseline);
    }

    /// Get the baseline for a command.
    #[must_use]
    pub fn get(&self, command: &str) -> Option<ContractBaseline> {
        let map = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.get(command).cloned()
    }

    /// Get all baselines.
    #[must_use]
    pub fn all(&self) -> Vec<ContractBaseline> {
        let map = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.values().cloned().collect()
    }

    /// Clear all baselines.
    pub fn clear(&self) -> usize {
        let mut map = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = map.len();
        map.clear();
        count
    }
}

impl Default for ContractStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Startup Profiling ───────────────────────────────────────────────────────

/// A single phase in the startup timeline.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct StartupPhase {
    /// Phase name.
    pub name: String,
    /// Duration of this phase in milliseconds.
    pub duration_ms: f64,
    /// Cumulative time from plugin init start.
    pub cumulative_ms: f64,
}

/// Records timestamps at key phases during plugin initialization.
pub struct StartupTimeline {
    start: Instant,
    phases: RwLock<Vec<(String, Instant)>>,
}

impl StartupTimeline {
    /// Begin recording from now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            phases: RwLock::new(Vec::new()),
        }
    }

    /// Mark a phase as completed.
    pub fn mark(&self, name: &str) {
        let mut phases = self
            .phases
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        phases.push((name.to_string(), Instant::now()));
    }

    /// Get the timeline as a list of phases with durations.
    #[must_use]
    pub fn report(&self) -> Vec<StartupPhase> {
        let phases = self
            .phases
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut result = Vec::new();
        let mut prev = self.start;

        for (name, instant) in phases.iter() {
            let duration = instant.duration_since(prev);
            let cumulative = instant.duration_since(self.start);
            result.push(StartupPhase {
                name: name.clone(),
                duration_ms: (duration.as_secs_f64() * 1000.0 * 100.0).round() / 100.0,
                cumulative_ms: (cumulative.as_secs_f64() * 1000.0 * 100.0).round() / 100.0,
            });
            prev = *instant;
        }
        result
    }

    /// Total time from start to last recorded phase.
    #[must_use]
    pub fn total_ms(&self) -> f64 {
        let phases = self
            .phases
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, last)) = phases.last() {
            (last.duration_since(self.start).as_secs_f64() * 1000.0 * 100.0).round() / 100.0
        } else {
            0.0
        }
    }
}

impl Default for StartupTimeline {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tauri Event Bus Monitor ─────────────────────────────────────────────

/// A Tauri event captured from the application's native event bus.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct CapturedTauriEvent {
    /// Event name (e.g. "notification-added", `tauri://focus`).
    pub name: String,
    /// Serialized event payload.
    pub payload: String,
    /// ISO 8601 timestamp.
    pub timestamp: String,
}

impl CapturedTauriEvent {
    /// Creates a captured event record (`timestamp` is an ISO 8601 string).
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        payload: impl Into<String>,
        timestamp: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            payload: payload.into(),
            timestamp: timestamp.into(),
        }
    }
}

const DEFAULT_EVENT_BUS_CAPACITY: usize = 1000;

/// Thread-safe ring buffer for captured Tauri events.
#[derive(Clone)]
pub struct EventBusMonitor {
    inner: std::sync::Arc<RwLock<VecDeque<CapturedTauriEvent>>>,
    capacity: usize,
}

impl EventBusMonitor {
    /// Create a new monitor with the given capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: std::sync::Arc::new(RwLock::new(VecDeque::with_capacity(capacity))),
            capacity,
        }
    }

    /// Record a captured event.
    pub fn push(&self, event: CapturedTauriEvent) {
        let mut buf = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if buf.len() >= self.capacity {
            buf.pop_front();
        }
        buf.push_back(event);
    }

    /// Get all captured events.
    #[must_use]
    pub fn events(&self) -> Vec<CapturedTauriEvent> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    /// Get the number of captured events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Returns true if no events have been captured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clear all captured events, returning how many were removed.
    pub fn clear(&self) -> usize {
        let mut buf = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = buf.len();
        buf.clear();
        count
    }
}

impl Default for EventBusMonitor {
    fn default() -> Self {
        Self::new(DEFAULT_EVENT_BUS_CAPACITY)
    }
}

// ── Application State Probes ─────────────────────────────────────────────

/// A named closure that returns a snapshot of application-specific backend state
/// as JSON. Registered via [`VictauriBuilder::probe`](crate::VictauriBuilder::probe).
pub type ProbeFn = dyn Fn() -> serde_json::Value + Send + Sync + 'static;

/// Registry of application-defined state probes surfaced through the `app_state`
/// MCP tool.
///
/// Probes give an agent first-class, discoverable access to domain state that
/// would otherwise require `query_db` + log-grepping (e.g. a scoring pipeline's
/// version, queue depth, or cache stats). Because a probe runs in the Rust
/// process with direct access to whatever state the app captured into it, it
/// reads backend state with **no IPC round-trip and no frontend involvement** —
/// the kind of introspection a browser-external tool like CDP cannot do.
#[derive(Clone, Default)]
pub struct AppStateProbes {
    inner: std::sync::Arc<RwLock<std::collections::BTreeMap<String, std::sync::Arc<ProbeFn>>>>,
}

impl AppStateProbes {
    /// Register (or replace) a probe under `name`.
    pub fn register(&self, name: impl Into<String>, probe: std::sync::Arc<ProbeFn>) {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(name.into(), probe);
    }

    /// Sorted list of registered probe names.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .cloned()
            .collect()
    }

    /// Run the named probe and return its JSON snapshot, or `None` if no probe is
    /// registered under that name.
    #[must_use]
    pub fn run(&self, name: &str) -> Option<serde_json::Value> {
        self.get(name).map(|p| p())
    }

    /// The probe registered under `name`, to run somewhere other than the caller's thread
    /// (the `app_state` tool runs it on the blocking pool with a timeout).
    #[must_use]
    pub fn get(&self, name: &str) -> Option<std::sync::Arc<ProbeFn>> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .cloned()
    }

    /// Number of registered probes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Returns true if no probes are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ── Internal Task Tracker ──────────────────────────────────────────────

/// Info about a tracked async task spawned by Victauri.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct TrackedTaskInfo {
    /// Human-readable task name.
    pub name: String,
    /// ISO 8601 timestamp when the task was spawned.
    pub spawned_at: String,
    /// Whether the task has finished (completed or errored).
    pub is_finished: bool,
    /// How long the task has been running in seconds.
    pub uptime_secs: u64,
}

struct TrackedTaskEntry {
    name: String,
    spawned_at: Instant,
    spawned_at_wall: String,
    finished: std::sync::Arc<AtomicBool>,
}

/// Tracks Victauri's own spawned async tasks for observability.
pub struct TaskTracker {
    tasks: RwLock<Vec<TrackedTaskEntry>>,
}

impl TaskTracker {
    /// Create a new empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tasks: RwLock::new(Vec::new()),
        }
    }

    /// Register a new task. Returns a flag that the task should set to `true` when it finishes.
    pub fn track(&self, name: &str) -> std::sync::Arc<AtomicBool> {
        let finished = std::sync::Arc::new(AtomicBool::new(false));
        let entry = TrackedTaskEntry {
            name: name.to_string(),
            spawned_at: Instant::now(),
            spawned_at_wall: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            finished: finished.clone(),
        };
        self.tasks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(entry);
        finished
    }

    /// List all tracked tasks with their current status.
    #[must_use]
    pub fn list(&self) -> Vec<TrackedTaskInfo> {
        let tasks = self
            .tasks
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tasks
            .iter()
            .map(|t| TrackedTaskInfo {
                name: t.name.clone(),
                spawned_at: t.spawned_at_wall.clone(),
                is_finished: t.finished.load(std::sync::atomic::Ordering::Relaxed),
                uptime_secs: t.spawned_at.elapsed().as_secs(),
            })
            .collect()
    }

    /// Count of active (non-finished) tasks.
    #[must_use]
    pub fn active_count(&self) -> usize {
        let tasks = self
            .tasks
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tasks
            .iter()
            .filter(|t| !t.finished.load(std::sync::atomic::Ordering::Relaxed))
            .count()
    }
}

impl Default for TaskTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ── Child Process Enumeration ──────────────────────────────────────────

/// Information about a child process of the Tauri application.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ChildProcessInfo {
    /// Process ID.
    pub pid: u32,
    /// Parent process ID.
    pub ppid: u32,
    /// Executable name (not full path).
    pub name: String,
    /// Memory usage in bytes (working set / RSS), if available.
    pub memory_bytes: Option<u64>,
}

/// Enumerate child processes of the current process.
///
/// Uses platform-native APIs:
/// - Windows: `CreateToolhelp32Snapshot` + `Process32First/Next`
/// - Linux: `/proc/` filesystem
/// - macOS: `proc_listpids` + `proc_pidinfo`
#[must_use]
pub fn enumerate_child_processes() -> Vec<ChildProcessInfo> {
    let my_pid = std::process::id();

    #[cfg(windows)]
    {
        enumerate_children_windows(my_pid)
    }

    #[cfg(target_os = "linux")]
    {
        enumerate_children_linux(my_pid)
    }

    #[cfg(target_os = "macos")]
    {
        enumerate_children_macos(my_pid)
    }

    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = my_pid;
        Vec::new()
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn enumerate_children_windows(parent_pid: u32) -> Vec<ChildProcessInfo> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32, Process32First, Process32Next, TH32CS_SNAPPROCESS,
    };

    let mut children = Vec::new();

    // SAFETY: `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)` creates a
    // read-only snapshot of all running processes. The returned handle is
    // closed via `CloseHandle` when we're done. `Process32First/Next` iterate
    // the snapshot entries.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return children;
        };

        let mut entry: PROCESSENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;

        if Process32First(snapshot, &mut entry).is_ok() {
            loop {
                if entry.th32ParentProcessID == parent_pid && entry.th32ProcessID != parent_pid {
                    let name_bytes: Vec<u8> = entry
                        .szExeFile
                        .iter()
                        .take_while(|&&b| b != 0)
                        .map(|&b| b as u8)
                        .collect();
                    let name = String::from_utf8_lossy(&name_bytes).to_string();

                    let memory_bytes = get_process_memory_windows(entry.th32ProcessID);

                    children.push(ChildProcessInfo {
                        pid: entry.th32ProcessID,
                        ppid: entry.th32ParentProcessID,
                        name,
                        memory_bytes,
                    });
                }

                if Process32Next(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }

        let _ = CloseHandle(snapshot);
    }

    children
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn get_process_memory_windows(pid: u32) -> Option<u64> {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };

    // SAFETY: `OpenProcess` with `PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ`
    // opens a limited handle for reading memory stats. The process handle is closed
    // automatically when dropped (windows crate handles this).
    unsafe {
        let process = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
            false,
            pid,
        )
        .ok()?;

        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;

        if GetProcessMemoryInfo(process, &mut counters, counters.cb).is_ok() {
            Some(counters.WorkingSetSize as u64)
        } else {
            None
        }
    }
}

#[cfg(target_os = "linux")]
fn enumerate_children_linux(parent_pid: u32) -> Vec<ChildProcessInfo> {
    let mut children = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return children;
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(pid_str) = file_name.to_str() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };

        let status_path = format!("/proc/{pid}/status");
        let Ok(status) = std::fs::read_to_string(&status_path) else {
            continue;
        };

        let mut ppid: Option<u32> = None;
        let mut name = String::new();
        let mut vm_rss_kb: u64 = 0;

        for line in status.lines() {
            if let Some(v) = line.strip_prefix("PPid:\t") {
                ppid = v.trim().parse().ok();
            } else if let Some(v) = line.strip_prefix("Name:\t") {
                name = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("VmRSS:") {
                vm_rss_kb = v
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
            }
        }

        if ppid == Some(parent_pid) {
            children.push(ChildProcessInfo {
                pid,
                ppid: parent_pid,
                name,
                memory_bytes: if vm_rss_kb > 0 {
                    Some(vm_rss_kb * 1024)
                } else {
                    None
                },
            });
        }
    }

    children
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn enumerate_children_macos(parent_pid: u32) -> Vec<ChildProcessInfo> {
    use std::mem;

    unsafe extern "C" {
        fn proc_listchildpids(ppid: i32, buffer: *mut i32, buffersize: i32) -> i32;
        fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut u8, buffersize: i32) -> i32;
        fn proc_name(pid: i32, buffer: *mut u8, buffersize: u32) -> i32;
    }

    const PROC_PIDTASKINFO: i32 = 4;

    #[repr(C)]
    struct ProcTaskInfo {
        pti_virtual_size: u64,
        pti_resident_size: u64,
        pti_total_user: u64,
        pti_total_system: u64,
        pti_threads_user: u64,
        pti_threads_system: u64,
        pti_policy: i32,
        pti_faults: i32,
        pti_pageins: i32,
        pti_cow_faults: i32,
        pti_messages_sent: i32,
        pti_messages_received: i32,
        pti_syscalls_mach: i32,
        pti_syscalls_unix: i32,
        pti_csw: i32,
        pti_threadnum: i32,
        pti_numrunning: i32,
        pti_priority: i32,
    }

    let mut children = Vec::new();

    // SAFETY: `proc_listchildpids` populates a buffer of child PIDs for the given
    // parent PID. We first call with a zero buffer to get the count, then allocate
    // and call again. `proc_name` and `proc_pidinfo` read metadata for a given PID.
    unsafe {
        let ppid = parent_pid as i32;
        // `proc_listchildpids` returns the COUNT of child PIDs written, not bytes —
        // verified empirically on macOS 26 / arm64 (1 child → returns 1, pids[0] is
        // the child). The old code divided the return by `size_of::<i32>()`, so a
        // single child (1/4 = 0) always enumerated as zero. Allocate a generous
        // buffer and call directly; if the returned count meets our capacity the list
        // may be truncated, so grow and retry.
        let mut cap = 256usize;
        let (pids, n) = loop {
            let mut pids = vec![0i32; cap];
            let buf_size = (cap * mem::size_of::<i32>()) as i32;
            let actual = proc_listchildpids(ppid, pids.as_mut_ptr(), buf_size);
            if actual <= 0 {
                return children;
            }
            let count = actual as usize;
            // `count` is clamped to `cap` for the slice below (stays in bounds even if
            // the syscall ever reports a total larger than the buffer it filled).
            if count < cap || cap >= 65536 {
                break (pids, count.min(cap));
            }
            cap = (count + 16).max(cap * 2);
        };
        for &pid in &pids[..n] {
            if pid <= 0 {
                continue;
            }

            let mut name_buf = [0u8; 256];
            let name_len = proc_name(pid, name_buf.as_mut_ptr(), 256);
            let name = if name_len > 0 {
                String::from_utf8_lossy(&name_buf[..name_len as usize]).to_string()
            } else {
                String::from("<unknown>")
            };

            let mut task_info: ProcTaskInfo = mem::zeroed();
            let info_size = mem::size_of::<ProcTaskInfo>() as i32;
            let ret = proc_pidinfo(
                pid,
                PROC_PIDTASKINFO,
                0,
                &mut task_info as *mut _ as *mut u8,
                info_size,
            );

            let memory_bytes = if ret == info_size {
                Some(task_info.pti_resident_size)
            } else {
                None
            };

            children.push(ChildProcessInfo {
                pid: pid as u32,
                ppid: parent_pid,
                name,
                memory_bytes,
            });
        }
    }

    children
}

// ── Page loads ─────────────────────────────────────────────────────────────

/// Each window's latest bridge ready signal — sent whenever its JS bridge (re)initializes, i.e.
/// on every page load or reload — with the nonce identifying that page load. An eval running in
/// a page that has since been replaced can never deliver its result, so a ready signal from a
/// DIFFERENT page lets the caller fail fast ("the page reloaded while the call was in flight")
/// instead of waiting out the full timeout.
#[derive(Default)]
pub(crate) struct PageLoads {
    last_load: std::sync::Mutex<HashMap<String, PageLoad>>,
    seq: std::sync::atomic::AtomicU64,
    changed: tokio::sync::Notify,
}

/// One recorded ready signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageLoad {
    /// Increases with every ready signal (from any window), so a caller can tell "a new signal
    /// since I looked" from "the same one".
    pub seq: u64,
    /// The page-load nonce the signal carried (`None` from a bridge that sends none).
    pub nonce: Option<String>,
}

/// Longest nonce kept from a ready signal (the bridge's is a UUID; the signal is page-callable).
const MAX_PAGE_NONCE_LEN: usize = 128;

impl PageLoads {
    /// Record that window `label` sent a ready signal for the page identified by `nonce`.
    pub fn record_load(&self, label: &str, nonce: Option<&str>) {
        let nonce = nonce.map(|n| n.chars().take(MAX_PAGE_NONCE_LEN).collect());
        {
            let mut loads = self
                .last_load
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Numbered under the lock, so a window's stored seq only ever grows.
            let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            loads.insert(label.to_string(), PageLoad { seq, nonce });
        }
        self.changed.notify_waiters();
    }

    /// Window `label`'s latest ready signal, if any.
    #[must_use]
    pub fn latest(&self, label: &str) -> Option<PageLoad> {
        self.last_load
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(label)
            .cloned()
    }

    /// The sequence number of the most recent ready signal from any window (0 if none yet).
    #[must_use]
    pub fn current_seq(&self) -> u64 {
        self.seq.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Resolves the next time any window records a page load.
    pub fn changed(&self) -> tokio::sync::futures::Notified<'_> {
        self.changed.notified()
    }
}
// ── Recording drain watermarks ─────────────────────────────────────────────

/// Per-window drain positions for pulling the JS bridge's event stream into a recording,
/// shared by the background drain loop and `recording flush`.
///
/// Each kept its own watermark before, so a flush re-recorded what the drain had captured
/// (and vice versa). A position is the bridge's per-page event SEQUENCE (plus the page
/// instance it belongs to), not a wall-clock timestamp: a timestamp watermark clamped to the
/// Rust clock re-read an event stamped ahead of it on every drain, and lost events pushed in
/// the same millisecond after a read. Positions are reset whenever a recording starts or is
/// imported — the new epoch is the recorder generation, and a window's first read in it skips
/// the page's pre-recording history by the `floor_ms` timestamp. A per-window async lock
/// serializes the two readers for the same window.
#[derive(Default)]
pub(crate) struct DrainWatermarks {
    inner: std::sync::Mutex<WatermarkState>,
}

#[derive(Default)]
struct WatermarkState {
    floor_ms: f64,
    epoch: u64,
    per_label: HashMap<String, DrainMark>,
    locks: HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>,
}

/// How far a window's event stream has been drained: the bridge's page `instance` and the
/// last sequence number read from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DrainMark {
    /// Identifies one page load of the window (the sequence restarts on reload).
    pub instance: String,
    /// The last sequence number read from that page.
    pub seq: u64,
}

/// Where the next drain of a window starts, captured in one step.
#[derive(Debug, Clone)]
pub(crate) struct DrainCursor {
    /// The recording epoch (recorder generation) this position belongs to.
    pub epoch: u64,
    /// Entries completed at or before this time predate the recording (first read only).
    pub floor_ms: f64,
    /// The window's position, or `None` if it has not been read in this epoch.
    pub mark: Option<DrainMark>,
}

impl DrainWatermarks {
    fn state(&self) -> std::sync::MutexGuard<'_, WatermarkState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Where to read `label`'s stream from.
    #[must_use]
    pub fn cursor(&self, label: &str) -> DrainCursor {
        let s = self.state();
        DrainCursor {
            epoch: s.epoch,
            floor_ms: s.floor_ms,
            mark: s.per_label.get(label).cloned(),
        }
    }

    /// Record that `label` was read up to `mark`, if `epoch` is still the current one (a read
    /// that straddled a reset must not plant an old position in the new epoch). Returns
    /// whether the position was stored.
    pub fn advance(&self, label: &str, epoch: u64, mark: DrainMark) -> bool {
        let mut s = self.state();
        if s.epoch != epoch {
            return false;
        }
        s.per_label.insert(label.to_string(), mark);
        true
    }

    /// Start recording epoch `epoch`: every window is read afresh, skipping entries at or
    /// before `floor_ms`. Monotonic: epochs are recorder generations (strictly increasing), so a
    /// reset for an epoch at or below the current one is a stale caller — a `start` whose
    /// handler ran late — and is ignored (R4-RACE1).
    pub fn reset(&self, floor_ms: f64, epoch: u64) {
        let mut s = self.state();
        if epoch <= s.epoch {
            return;
        }
        s.floor_ms = floor_ms;
        s.epoch = epoch;
        s.per_label.clear();
    }

    /// Forget windows that no longer exist (bounds the maps across ephemeral windows).
    pub fn retain(&self, labels: &[String]) {
        let mut s = self.state();
        s.per_label.retain(|l, _| labels.contains(l));
        s.locks.retain(|l, _| labels.contains(l));
    }

    /// The lock that serializes readers of `label`'s stream.
    #[must_use]
    pub fn lock_for(&self, label: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        std::sync::Arc::clone(self.state().locks.entry(label.to_string()).or_default())
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// R4-RACE1: a delayed reset for an OLDER recording generation moved the epoch backwards,
    /// so the drain for the current recording (which captured the newer epoch) had every
    /// `advance` refused and the recording silently captured no webview events.
    #[test]
    fn drain_watermark_reset_is_monotonic() {
        let w = DrainWatermarks::default();
        w.reset(200.0, 2);
        let mark = DrainMark {
            instance: "page-a".to_string(),
            seq: 7,
        };
        assert!(w.advance("main", 2, mark.clone()));
        // The reset for generation 1 (an earlier `start` whose handler was delayed) lands late.
        w.reset(100.0, 1);
        let c = w.cursor("main");
        assert_eq!(
            c.epoch, 2,
            "a stale reset must not move the epoch backwards"
        );
        assert!((c.floor_ms - 200.0).abs() < f64::EPSILON);
        assert_eq!(c.mark, Some(mark));
        // The current recording's drain keeps advancing.
        assert!(w.advance(
            "main",
            2,
            DrainMark {
                instance: "page-a".to_string(),
                seq: 9,
            }
        ));
        // A newer generation still resets.
        w.reset(300.0, 3);
        let c = w.cursor("main");
        assert_eq!((c.epoch, c.mark), (3, None));
    }

    // C15e: the map is keyed by the caller-chosen command name, so it must be bounded.
    #[test]
    fn command_timings_bound_distinct_commands() {
        let t = CommandTimings::new();
        for i in 0..(MAX_TIMED_COMMANDS + 100) {
            t.record(&format!("cmd-{i}"), Duration::from_millis(1));
        }
        assert_eq!(t.all_stats().len(), MAX_TIMED_COMMANDS);
        // Commands already tracked keep accumulating once the cap is reached.
        t.record("cmd-0", Duration::from_millis(1));
        assert_eq!(t.stats_for("cmd-0").unwrap().count, 2);
    }

    #[test]
    fn p95_is_nearest_rank_not_max_for_small_samples() {
        let t = CommandTimings::new();
        for ms in 1..=20 {
            t.record("cmd", Duration::from_millis(ms));
        }
        let s = t.stats_for("cmd").unwrap();
        assert!(
            (s.p95_ms - 19.0).abs() < 1e-9,
            "p95 of 1..=20 is 19, got {}",
            s.p95_ms
        );
        assert!((s.max_ms - 20.0).abs() < 1e-9);
    }

    #[test]
    fn app_state_probes_register_run_list() {
        let probes = AppStateProbes::default();
        assert!(probes.is_empty());

        probes.register(
            "scoring",
            std::sync::Arc::new(|| serde_json::json!({ "pipeline_version": 5 })),
        );
        probes.register("queue", std::sync::Arc::new(|| serde_json::json!(0)));

        // names() is sorted (BTreeMap).
        assert_eq!(
            probes.names(),
            vec!["queue".to_string(), "scoring".to_string()]
        );
        assert_eq!(probes.len(), 2);

        let snapshot = probes.run("scoring").expect("probe runs");
        assert_eq!(snapshot["pipeline_version"], 5);
        assert!(probes.run("missing").is_none());
    }

    #[test]
    fn app_state_probe_reflects_live_state() {
        // A probe closes over shared state and reflects mutations at call time —
        // proving it reads live backend state, not a registration-time snapshot.
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let probe_counter = std::sync::Arc::clone(&counter);
        let probes = AppStateProbes::default();
        probes.register(
            "counter",
            std::sync::Arc::new(move || {
                serde_json::json!(probe_counter.load(std::sync::atomic::Ordering::SeqCst))
            }),
        );

        assert_eq!(probes.run("counter").unwrap(), serde_json::json!(0));
        counter.store(42, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(probes.run("counter").unwrap(), serde_json::json!(42));
    }

    #[test]
    fn event_bus_push_and_read() {
        let bus = EventBusMonitor::new(3);
        assert!(bus.is_empty());
        bus.push(CapturedTauriEvent {
            name: "test".to_string(),
            payload: "{}".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        });
        assert_eq!(bus.len(), 1);
        assert_eq!(bus.events()[0].name, "test");
    }

    #[test]
    fn event_bus_ring_buffer_eviction() {
        let bus = EventBusMonitor::new(2);
        for i in 0..5 {
            bus.push(CapturedTauriEvent {
                name: format!("event_{i}"),
                payload: String::new(),
                timestamp: String::new(),
            });
        }
        assert_eq!(bus.len(), 2);
        assert_eq!(bus.events()[0].name, "event_3");
        assert_eq!(bus.events()[1].name, "event_4");
    }

    #[test]
    fn event_bus_clear() {
        let bus = EventBusMonitor::new(10);
        bus.push(CapturedTauriEvent {
            name: "a".to_string(),
            payload: String::new(),
            timestamp: String::new(),
        });
        assert_eq!(bus.clear(), 1);
        assert!(bus.is_empty());
    }

    #[test]
    fn task_tracker_lifecycle() {
        let tracker = TaskTracker::new();
        let flag = tracker.track("mcp_server");
        let tasks = tracker.list();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "mcp_server");
        assert!(!tasks[0].is_finished);
        assert_eq!(tracker.active_count(), 1);

        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        let tasks = tracker.list();
        assert!(tasks[0].is_finished);
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn timing_samples_basic() {
        let mut samples = TimingSamples::default();
        samples.record(Duration::from_millis(10));
        samples.record(Duration::from_millis(20));
        samples.record(Duration::from_millis(30));
        let stats = samples.stats("test_cmd");
        assert_eq!(stats.count, 3);
        assert!((stats.min_ms - 10.0).abs() < 1.0);
        assert!((stats.max_ms - 30.0).abs() < 1.0);
        assert!((stats.avg_ms - 20.0).abs() < 1.0);
    }

    #[test]
    fn timing_samples_empty() {
        let samples = TimingSamples::default();
        let stats = samples.stats("empty");
        assert_eq!(stats.count, 0);
        assert_eq!(stats.min_ms, 0.0);
    }

    #[test]
    fn timing_samples_bounded_but_count_accurate() {
        // Record far more than the ring capacity. Memory must stay bounded
        // (recent window <= MAX_TIMING_SAMPLES) while count/min/max/total remain
        // accurate over the FULL history — not just the retained window. This test
        // fails if the cap is removed (unbounded growth) or if the running
        // aggregates are dropped in favor of windowed-only stats.
        let mut samples = TimingSamples::default();
        let n = MAX_TIMING_SAMPLES * 3;
        for i in 0..n {
            // Durations 1..=n ms; the smallest (1ms) and largest (n ms) fall
            // outside the retained recent window, proving min/max are all-time.
            samples.record(Duration::from_millis((i + 1) as u64));
        }
        assert!(
            samples.recent.len() <= MAX_TIMING_SAMPLES,
            "recent window must stay bounded, got {}",
            samples.recent.len()
        );
        let stats = samples.stats("soak");
        assert_eq!(stats.count, n as u64, "count must reflect full history");
        assert!(
            (stats.min_ms - 1.0).abs() < 0.5,
            "all-time min lost: {}",
            stats.min_ms
        );
        assert!(
            (stats.max_ms - n as f64).abs() < 0.5,
            "all-time max lost: {}",
            stats.max_ms
        );
    }

    #[test]
    fn command_timings_thread_safe() {
        let timings = CommandTimings::new();
        timings.record("cmd_a", Duration::from_millis(5));
        timings.record("cmd_a", Duration::from_millis(15));
        timings.record("cmd_b", Duration::from_millis(100));

        let all = timings.all_stats();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].command, "cmd_b");

        let a = timings.stats_for("cmd_a").unwrap();
        assert_eq!(a.count, 2);
    }

    #[test]
    fn fault_registry_lifecycle() {
        let registry = FaultRegistry::new();
        registry.inject(FaultConfig {
            command: "slow_cmd".to_string(),
            fault_type: FaultType::Delay { delay_ms: 500 },
            trigger_count: 0,
            max_triggers: 2,
            created_at: Instant::now(),
        });

        assert!(registry.check_and_trigger("slow_cmd").is_some());
        assert!(registry.check_and_trigger("slow_cmd").is_some());
        assert!(registry.check_and_trigger("slow_cmd").is_none());

        assert_eq!(registry.list().len(), 1);
        assert!(registry.clear("slow_cmd"));
        assert_eq!(registry.list().len(), 0);
    }

    #[test]
    fn fault_registry_unlimited() {
        let registry = FaultRegistry::new();
        registry.inject(FaultConfig {
            command: "always_fail".to_string(),
            fault_type: FaultType::Error {
                message: "injected".to_string(),
            },
            trigger_count: 0,
            max_triggers: 0,
            created_at: Instant::now(),
        });

        for _ in 0..100 {
            assert!(registry.check_and_trigger("always_fail").is_some());
        }
    }

    #[test]
    fn fault_expires_after_ttl() {
        let cfg = FaultConfig {
            command: "x".to_string(),
            fault_type: FaultType::Error {
                message: "e".to_string(),
            },
            trigger_count: 0,
            max_triggers: 0, // unlimited by count...
            created_at: Instant::now(),
        };
        // ...but still inert once older than the TTL (audit #34).
        assert!(cfg.should_trigger_at(cfg.created_at));
        assert!(!cfg.should_trigger_at(cfg.created_at + FAULT_TTL + Duration::from_secs(1)));
    }

    #[test]
    fn json_shape_extraction() {
        let value = serde_json::json!({
            "name": "test",
            "count": 42,
            "active": true,
            "items": [{"id": 1}],
            "meta": null
        });
        let shape = JsonShape::from_value(&value);
        match &shape {
            JsonShape::Object(fields) => {
                assert_eq!(fields.len(), 5);
                assert_eq!(*fields.get("name").unwrap(), JsonShape::String);
                assert_eq!(*fields.get("count").unwrap(), JsonShape::Number);
                assert_eq!(*fields.get("active").unwrap(), JsonShape::Bool);
                assert_eq!(*fields.get("meta").unwrap(), JsonShape::Null);
            }
            _ => panic!("expected object"),
        }
    }

    #[test]
    fn contract_diff_detects_changes() {
        let baseline = serde_json::json!({"name": "old", "count": 1});
        let current = serde_json::json!({"name": "new", "count": "not_a_number", "extra": true});

        let b_shape = JsonShape::from_value(&baseline);
        let c_shape = JsonShape::from_value(&current);
        let drift = diff_shapes(&b_shape, &c_shape, "test_cmd");

        assert!(!drift.shape_matches);
        assert_eq!(drift.new_fields, vec!["test_cmd.extra"]);
        assert_eq!(drift.type_changes.len(), 1);
        assert_eq!(drift.type_changes[0].path, "test_cmd.count");
    }

    #[test]
    fn contract_store_crud() {
        let store = ContractStore::new();
        let baseline = ContractBaseline {
            command: "get_user".to_string(),
            args: serde_json::json!({}),
            shape: JsonShape::Object(HashMap::new()),
            sample: "{}".to_string(),
            recorded_at: "2026-05-26".to_string(),
        };
        store.record(baseline);
        assert!(store.get("get_user").is_some());
        assert_eq!(store.all().len(), 1);
        assert_eq!(store.clear(), 1);
        assert!(store.get("get_user").is_none());
    }

    #[test]
    fn startup_timeline_records_phases() {
        let timeline = StartupTimeline::new();
        std::thread::sleep(Duration::from_millis(5));
        timeline.mark("phase_1");
        std::thread::sleep(Duration::from_millis(5));
        timeline.mark("phase_2");

        let report = timeline.report();
        assert_eq!(report.len(), 2);
        assert_eq!(report[0].name, "phase_1");
        assert!(report[1].cumulative_ms >= report[0].cumulative_ms);
        assert!(timeline.total_ms() > 0.0);
    }

    #[test]
    fn enumerate_child_processes_returns_vec() {
        let children = enumerate_child_processes();
        // The test process itself may or may not have children, but the
        // function must not panic and must return a well-formed Vec.
        for child in &children {
            assert_ne!(child.pid, 0, "child PID should be non-zero");
            assert_eq!(
                child.ppid,
                std::process::id(),
                "parent PID should match current process"
            );
            assert!(!child.name.is_empty(), "child name should not be empty");
        }
    }

    #[test]
    fn enumerate_child_processes_with_spawned_child() {
        // Spawn a short-lived child process and verify we can enumerate it.
        let child = std::process::Command::new(if cfg!(windows) { "cmd.exe" } else { "sleep" })
            .args(if cfg!(windows) {
                &["/c", "timeout /t 10 /nobreak >nul"][..]
            } else {
                &["10"][..]
            })
            .spawn();

        if let Ok(mut child_proc) = child {
            let children = enumerate_child_processes();
            assert!(
                !children.is_empty(),
                "should find at least one child process"
            );

            let found = children.iter().any(|c| c.pid == child_proc.id());
            assert!(
                found,
                "spawned child (PID {}) should appear in enumeration",
                child_proc.id()
            );

            let _ = child_proc.kill();
            let _ = child_proc.wait();
        }
    }

    #[test]
    fn child_process_info_serializes() {
        let info = ChildProcessInfo {
            pid: 1234,
            ppid: 5678,
            name: "test-sidecar".to_string(),
            memory_bytes: Some(1_048_576),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["pid"], 1234);
        assert_eq!(json["ppid"], 5678);
        assert_eq!(json["name"], "test-sidecar");
        assert_eq!(json["memory_bytes"], 1_048_576);
    }

    #[test]
    fn child_process_info_serializes_no_memory() {
        let info = ChildProcessInfo {
            pid: 42,
            ppid: 1,
            name: "zombie".to_string(),
            memory_bytes: None,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert!(json["memory_bytes"].is_null());
    }
}
