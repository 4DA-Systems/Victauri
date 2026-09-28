//! Screencast state for the `trace` tool — a ring buffer of timestamped PNG
//! frames captured by a background task at a fixed interval. Pairs with the
//! `EventRecorder` (events) and `logs` (network/console) to form a trace bundle.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Cap on the total base64 bytes buffered across all frames. `max_frames` alone does not bound
/// memory: 600 frames of a 4K window is well over a gigabyte. The oldest frames are evicted
/// first once this is exceeded.
pub const MAX_TRACE_BYTES: usize = 256 * 1024 * 1024;

/// Longest allowed capture interval. The capture task checks the trace's maximum duration once
/// per interval, so an unbounded interval (hours) defeated the 30-minute auto-stop.
pub const MAX_INTERVAL_MS: u64 = 60_000;

/// A single captured frame: milliseconds since trace start + base64 PNG.
#[derive(Debug, Clone, serde::Serialize)]
#[non_exhaustive]
pub struct TraceFrame {
    /// Milliseconds since the trace started.
    pub t_ms: u64,
    /// Base64-encoded PNG image data.
    pub data_b64: String,
}

/// Shared screencast state. Thread-safe; mutex locks are short-lived and
/// recover from poisoning.
#[derive(Debug)]
pub struct Screencast {
    interval_ms: AtomicU64,
    max_frames: AtomicUsize,
    /// Active flag, generation and owned recording change together under ONE lock: as separate
    /// atomics a `stop` landing inside a `start` left the trace active under a generation no
    /// capture task was running for, and an old task's auto-stop could take the recording a
    /// newer trace owned.
    trace: Mutex<TraceState>,
    frames: Mutex<Vec<TraceFrame>>,
    label: Mutex<Option<String>>,
}

#[derive(Debug, Default)]
struct TraceState {
    active: bool,
    generation: u64,
    /// Recorder generation of the recording this trace started (`with_events`), so stopping
    /// the trace stops exactly that one — otherwise the recorder (and the per-second drain
    /// loop it enables) outlives the trace. A session id could name a recording someone else
    /// started later with the same id.
    owned_recording: Option<u64>,
}

impl Default for Screencast {
    fn default() -> Self {
        Self {
            interval_ms: AtomicU64::new(500),
            max_frames: AtomicUsize::new(60),
            trace: Mutex::new(TraceState::default()),
            frames: Mutex::new(Vec::new()),
            label: Mutex::new(None),
        }
    }
}

impl Screencast {
    fn trace(&self) -> std::sync::MutexGuard<'_, TraceState> {
        self.trace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Begin a new trace: clears frames, records settings (the interval clamped to
    /// 50..=[`MAX_INTERVAL_MS`]), and returns the generation token the capture task must check
    /// to know it is current, plus the recording owned by the trace this one superseded (the
    /// caller stops it).
    pub fn start(
        &self,
        interval_ms: u64,
        max_frames: usize,
        label: Option<String>,
    ) -> (u64, Option<u64>) {
        let mut t = self.trace();
        self.interval_ms
            .store(interval_ms.clamp(50, MAX_INTERVAL_MS), Ordering::Relaxed);
        self.max_frames
            .store(max_frames.clamp(1, 600), Ordering::Relaxed);
        self.frames
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        *self
            .label
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = label;
        t.generation += 1;
        t.active = true;
        (t.generation, t.owned_recording.take())
    }

    /// Stop the current trace. Returns the captured frame count and the recording the trace
    /// owned (the caller stops it).
    pub fn stop(&self) -> (usize, Option<u64>) {
        let owned = {
            let mut t = self.trace();
            t.active = false;
            // Invalidate any running task.
            t.generation += 1;
            t.owned_recording.take()
        };
        (self.frame_count(), owned)
    }

    /// Whether a trace is currently active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.trace().active
    }

    /// The current generation token (a capture task is stale if it differs).
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.trace().generation
    }

    /// Whether the trace of `generation` is still the active one.
    #[must_use]
    pub fn is_current(&self, generation: u64) -> bool {
        let t = self.trace();
        t.active && t.generation == generation
    }

    /// Configured capture interval in milliseconds.
    #[must_use]
    pub fn interval_ms(&self) -> u64 {
        self.interval_ms.load(Ordering::Relaxed)
    }

    /// Target webview label for capture, if set.
    #[must_use]
    pub fn label(&self) -> Option<String> {
        self.label
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Append a frame, enforcing the `max_frames` ring-buffer cap and [`MAX_TRACE_BYTES`].
    pub fn push_frame(&self, t_ms: u64, data_b64: String) {
        self.push_frame_capped(t_ms, data_b64, MAX_TRACE_BYTES);
    }

    fn push_frame_capped(&self, t_ms: u64, data_b64: String, max_bytes: usize) {
        let max = self.max_frames.load(Ordering::Relaxed);
        let mut f = self
            .frames
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f.push(TraceFrame { t_ms, data_b64 });
        let len = f.len();
        if len > max {
            f.drain(0..len - max);
        }
        let mut total: usize = f.iter().map(|fr| fr.data_b64.len()).sum();
        let mut evict = 0;
        // Always keep the newest frame, even if it alone exceeds the cap.
        while total > max_bytes && evict + 1 < f.len() {
            total -= f[evict].data_b64.len();
            evict += 1;
        }
        f.drain(0..evict);
    }

    /// Append a frame only if `generation` is still current. The capture task checks the
    /// generation before a (slow) capture; re-checking at push time stops a stale task from
    /// inserting a frame into a trace started after it. The check and the push happen under
    /// the trace lock (taken before the frames lock, the same order as [`start`](Self::start)),
    /// so a `start` cannot clear the buffer between them and receive the stale frame (R4-RACE2).
    pub fn push_frame_if_current(&self, generation: u64, t_ms: u64, data_b64: String) -> bool {
        let t = self.trace();
        if !(t.active && t.generation == generation) {
            return false;
        }
        self.push_frame(t_ms, data_b64);
        drop(t);
        true
    }

    /// Record that the trace of `generation` started the recording of `recorder_generation`.
    /// Returns `false` (recording nothing) if that trace was already stopped or superseded —
    /// the caller then stops the recording itself, or nothing ever would.
    pub fn set_owned_recording(&self, generation: u64, recorder_generation: u64) -> bool {
        let mut t = self.trace();
        if t.active && t.generation == generation {
            t.owned_recording = Some(recorder_generation);
            true
        } else {
            false
        }
    }

    /// Stop the trace only if `generation` is still current — atomically, so an old capture
    /// task's auto-stop can never stop a NEWER trace started in between (or take the recording
    /// that trace owns). Returns `None` if it stopped nothing, else the stopped trace's owned
    /// recording.
    pub fn stop_if_generation(&self, generation: u64) -> Option<Option<u64>> {
        let mut t = self.trace();
        if t.active && t.generation == generation {
            t.active = false;
            t.generation += 1;
            Some(t.owned_recording.take())
        } else {
            None
        }
    }

    /// Number of frames currently buffered.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Return up to `limit` of the most recent frames (or all if `limit` is 0).
    #[must_use]
    pub fn frames(&self, limit: usize) -> Vec<TraceFrame> {
        let f = self
            .frames
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if limit == 0 || limit >= f.len() {
            f.clone()
        } else {
            f[f.len() - limit..].to_vec()
        }
    }

    /// Frame timestamps (ms since start) without the image payloads.
    #[must_use]
    pub fn frame_timestamps(&self) -> Vec<u64> {
        self.frames
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|fr| fr.t_ms)
            .collect()
    }
}

/// Ends the trace of `generation` when its capture task finishes — normally, at the maximum
/// duration, or by PANICKING (a drop guard runs during unwinding; a panicked task used to leave
/// the trace active and its recording running forever). A no-op if the trace was already
/// stopped or superseded.
pub(crate) struct CaptureTaskGuard {
    pub(crate) screencast: std::sync::Arc<Screencast>,
    pub(crate) recorder: victauri_core::EventRecorder,
    pub(crate) generation: u64,
}

impl Drop for CaptureTaskGuard {
    fn drop(&mut self) {
        if let Some(Some(owned)) = self.screencast.stop_if_generation(self.generation) {
            let _ = self.recorder.stop_if_generation(owned);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_caps_frames() {
        let sc = Screencast::default();
        sc.start(100, 3, None);
        for i in 0..5 {
            sc.push_frame(i * 100, format!("frame{i}"));
        }
        assert_eq!(sc.frame_count(), 3, "should cap at max_frames");
        let frames = sc.frames(0);
        // Oldest dropped: keeps frame2, frame3, frame4.
        assert_eq!(frames[0].data_b64, "frame2");
        assert_eq!(frames[2].data_b64, "frame4");
    }

    #[test]
    fn start_clears_and_bumps_generation() {
        let sc = Screencast::default();
        let (g1, _) = sc.start(200, 10, Some("main".into()));
        sc.push_frame(0, "x".into());
        assert_eq!(sc.frame_count(), 1);
        let (g2, _) = sc.start(200, 10, None);
        assert!(g2 > g1, "generation must increase");
        assert_eq!(sc.frame_count(), 0, "start clears frames");
        assert!(sc.is_active());
    }

    #[test]
    fn stop_deactivates_and_invalidates() {
        let sc = Screencast::default();
        let (g, _) = sc.start(200, 10, None);
        sc.stop();
        assert!(!sc.is_active());
        assert!(sc.generation() > g, "stop invalidates the task generation");
    }

    #[test]
    fn stale_generation_push_is_rejected() {
        let sc = Screencast::default();
        let (old, _) = sc.start(100, 10, None);
        let (new, _) = sc.start(100, 10, None);
        assert!(!sc.push_frame_if_current(old, 0, "stale".into()));
        assert!(sc.push_frame_if_current(new, 0, "fresh".into()));
        assert_eq!(sc.frames(0)[0].data_b64, "fresh");
        sc.stop();
        assert!(!sc.push_frame_if_current(new, 1, "after-stop".into()));
    }

    #[test]
    fn byte_cap_evicts_oldest_but_keeps_newest() {
        let sc = Screencast::default();
        sc.start(100, 600, None);
        let cap = 100;
        sc.push_frame_capped(0, "x".repeat(51), cap);
        sc.push_frame_capped(1, "y".repeat(51), cap);
        sc.push_frame_capped(2, "tail".into(), cap);
        let frames = sc.frames(0);
        let total: usize = frames.iter().map(|f| f.data_b64.len()).sum();
        assert!(total <= cap, "total {total} exceeds the cap");
        assert_eq!(frames.len(), 2, "only the oldest frame should be evicted");
        assert_eq!(frames.last().unwrap().data_b64, "tail");
    }

    #[test]
    fn owned_recording_is_taken_once() {
        let sc = Screencast::default();
        let (g, _) = sc.start(100, 10, None);
        assert!(sc.set_owned_recording(g, 7));
        assert_eq!(sc.stop(), (0, Some(7)));
        assert_eq!(sc.stop(), (0, None));
    }

    // C14b: ownership is scoped to the trace generation. An old task's auto-stop (after its
    // stop_if_generation) used to `take_owned_recording()` separately and could take the
    // recording a newer trace had just registered.
    #[test]
    fn owned_recording_belongs_to_its_trace_generation() {
        let sc = Screencast::default();
        let (old, _) = sc.start(100, 10, None);
        assert!(sc.set_owned_recording(old, 1));
        // A newer trace supersedes the old one and is handed the old trace's recording.
        let (new, superseded) = sc.start(100, 10, None);
        assert_eq!(superseded, Some(1));
        assert!(sc.set_owned_recording(new, 2));
        // The old task's auto-stop stops nothing and takes nothing.
        assert_eq!(sc.stop_if_generation(old), None);
        // A stale trace cannot register a recording (its starter must stop it itself).
        assert!(!sc.set_owned_recording(old, 3));
        assert_eq!(sc.stop_if_generation(new), Some(Some(2)));
    }

    // C14c: the interval is clamped so the per-interval max-duration check keeps running.
    #[test]
    fn interval_is_clamped() {
        let sc = Screencast::default();
        let _ = sc.start(u64::MAX, 10, None);
        assert_eq!(sc.interval_ms(), MAX_INTERVAL_MS);
        let _ = sc.start(1, 10, None);
        assert_eq!(sc.interval_ms(), 50);
    }

    // C14: a panicking capture task must still end its trace and the recording it started.
    #[test]
    fn capture_task_guard_cleans_up_after_a_panic() {
        let sc = std::sync::Arc::new(Screencast::default());
        let recorder = victauri_core::EventRecorder::new(10);
        let (g, _) = sc.start(100, 10, None);
        let rg = recorder.start_session("trace".into()).unwrap();
        assert!(sc.set_owned_recording(g, rg));
        let guard = CaptureTaskGuard {
            screencast: std::sync::Arc::clone(&sc),
            recorder: recorder.clone(),
            generation: g,
        };
        let r = std::thread::spawn(move || {
            let _guard = guard;
            panic!("capture blew up");
        })
        .join();
        assert!(r.is_err());
        assert!(!sc.is_active(), "trace left active after its task panicked");
        assert!(!recorder.is_recording(), "owned recording left running");
    }

    // A guard of a superseded trace touches neither the newer trace nor its recording.
    #[test]
    fn capture_task_guard_of_a_stale_trace_is_a_no_op() {
        let sc = std::sync::Arc::new(Screencast::default());
        let recorder = victauri_core::EventRecorder::new(10);
        let (old, _) = sc.start(100, 10, None);
        let (new, _) = sc.start(100, 10, None);
        let rg = recorder.start_session("new".into()).unwrap();
        assert!(sc.set_owned_recording(new, rg));
        drop(CaptureTaskGuard {
            screencast: std::sync::Arc::clone(&sc),
            recorder: recorder.clone(),
            generation: old,
        });
        assert!(sc.is_current(new));
        assert!(recorder.is_recording());
    }

    #[test]
    fn stale_generation_cannot_stop_a_newer_trace() {
        let sc = Screencast::default();
        let (old, _) = sc.start(100, 10, None);
        let (new, _) = sc.start(100, 10, None);
        assert!(
            sc.stop_if_generation(old).is_none(),
            "an old task must not stop the new trace"
        );
        assert!(sc.is_active());
        assert!(sc.stop_if_generation(new).is_some());
        assert!(!sc.is_active());
    }

    // C14a: a stop racing a start must never leave the trace active under a generation no
    // start returned (active, but with no capture task running for it).
    #[test]
    fn concurrent_start_and_stop_never_orphan_an_active_trace() {
        let sc = std::sync::Arc::new(Screencast::default());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let (sc2, b2) = (std::sync::Arc::clone(&sc), std::sync::Arc::clone(&barrier));
        let stopper = std::thread::spawn(move || {
            for _ in 0..20_000 {
                b2.wait();
                let _ = sc2.stop();
                b2.wait();
            }
        });
        let mut orphaned = 0;
        for _ in 0..20_000 {
            barrier.wait();
            let (g, _) = sc.start(100, 10, None);
            barrier.wait();
            if sc.is_active() && sc.generation() != g {
                orphaned += 1;
            }
        }
        stopper.join().unwrap();
        assert_eq!(orphaned, 0, "active trace with no capture task");
    }

    /// R4-RACE2: `push_frame_if_current` checked the generation, RELEASED the trace lock, then
    /// pushed — so a `start` landing in between cleared the buffer and the stale task's frame
    /// then landed in the NEW trace. Every frame is tagged with the generation that pushed it;
    /// right after a `start` returns, the buffer may only hold frames of that generation.
    #[test]
    fn stale_frame_never_lands_in_a_newer_trace() {
        let sc = std::sync::Arc::new(Screencast::default());
        let (first, _) = sc.start(100, 600, None);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pushers: Vec<_> = (0..3)
            .map(|_| {
                let (sc, stop) = (std::sync::Arc::clone(&sc), std::sync::Arc::clone(&stop));
                std::thread::spawn(move || {
                    let mut g = first;
                    while !stop.load(Ordering::Relaxed) {
                        if !sc.push_frame_if_current(g, 0, g.to_string()) {
                            g = sc.generation();
                        }
                    }
                })
            })
            .collect();
        let mut leaked = 0;
        for _ in 0..20_000 {
            let (g, _) = sc.start(100, 600, None);
            leaked += sc
                .frames(0)
                .iter()
                .filter(|f| f.data_b64.parse::<u64>().unwrap() != g)
                .count();
        }
        stop.store(true, Ordering::Relaxed);
        for p in pushers {
            p.join().unwrap();
        }
        assert_eq!(
            leaked, 0,
            "frames from a superseded trace leaked into a newer one"
        );
    }

    #[test]
    fn frames_limit_returns_most_recent() {
        let sc = Screencast::default();
        sc.start(100, 100, None);
        for i in 0..5 {
            sc.push_frame(i, format!("f{i}"));
        }
        let last2 = sc.frames(2);
        assert_eq!(last2.len(), 2);
        assert_eq!(last2[0].data_b64, "f3");
        assert_eq!(last2[1].data_b64, "f4");
    }
}
