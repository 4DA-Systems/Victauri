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

/// Accumulated raw timing samples for a single command.
#[derive(Debug, Default)]
pub struct TimingSamples {
    /// Duration of each invocation, in order.
    pub samples: Vec<Duration>,
}

impl TimingSamples {
    /// Add a timing sample.
    pub fn record(&mut self, duration: Duration) {
        self.samples.push(duration);
    }

    /// Compute aggregate statistics.
    #[must_use]
    pub fn stats(&self, command: &str) -> CommandTimingStats {
        if self.samples.is_empty() {
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
        let mut sorted: Vec<f64> = self
            .samples
            .iter()
            .map(|d| d.as_secs_f64() * 1000.0)
            .collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let count = sorted.len() as u64;
        let total: f64 = sorted.iter().sum();
        let min = sorted[0];
        let max = sorted[sorted.len() - 1];
        let avg = total / sorted.len() as f64;
        let p95_idx = ((sorted.len() as f64) * 0.95).ceil() as usize;
        let p95 = sorted[p95_idx.min(sorted.len() - 1)];

        CommandTimingStats {
            command: command.to_string(),
            count,
            min_ms: (min * 100.0).round() / 100.0,
            max_ms: (max * 100.0).round() / 100.0,
            avg_ms: (avg * 100.0).round() / 100.0,
            p95_ms: (p95 * 100.0).round() / 100.0,
            total_ms: (total * 100.0).round() / 100.0,
        }
    }
}

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
        map.entry(command.to_string()).or_default().record(duration);
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

impl FaultConfig {
    /// Check if this fault should still trigger (based on `max_triggers`).
    #[must_use]
    pub fn should_trigger(&self) -> bool {
        self.max_triggers == 0 || self.trigger_count < self.max_triggers
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

/// Differences found when checking a contract against baseline.
#[derive(Debug, Clone, Serialize)]
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
pub struct CapturedTauriEvent {
    /// Event name (e.g. "notification-added", `tauri://focus`).
    pub name: String,
    /// Serialized event payload.
    pub payload: String,
    /// ISO 8601 timestamp.
    pub timestamp: String,
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

// ── Internal Task Tracker ──────────────────────────────────────────────

/// Info about a tracked async task spawned by Victauri.
#[derive(Debug, Clone, Serialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
