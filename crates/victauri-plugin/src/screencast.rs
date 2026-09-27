//! Screencast state for the `trace` tool — a ring buffer of timestamped PNG
//! frames captured by a background task at a fixed interval. Pairs with the
//! `EventRecorder` (events) and `logs` (network/console) to form a trace bundle.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// Cap on the total base64 bytes buffered across all frames. `max_frames` alone does not bound
/// memory: 600 frames of a 4K window is well over a gigabyte. The oldest frames are evicted
/// first once this is exceeded.
pub const MAX_TRACE_BYTES: usize = 256 * 1024 * 1024;

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
    active: AtomicBool,
    interval_ms: AtomicU64,
    max_frames: AtomicUsize,
    generation: AtomicU64,
    frames: Mutex<Vec<TraceFrame>>,
    label: Mutex<Option<String>>,
    /// Session id of the recording this trace started (`with_events`), so `stop` can stop
    /// exactly that one — otherwise the recorder (and the per-second drain loop it enables)
    /// outlives the trace. A bare flag could stop a recording someone else started later.
    owned_recording: Mutex<Option<String>>,
}

impl Default for Screencast {
    fn default() -> Self {
        Self {
            active: AtomicBool::new(false),
            interval_ms: AtomicU64::new(500),
            max_frames: AtomicUsize::new(60),
            generation: AtomicU64::new(0),
            frames: Mutex::new(Vec::new()),
            label: Mutex::new(None),
            owned_recording: Mutex::new(None),
        }
    }
}

impl Screencast {
    /// Begin a new trace: clears frames, records settings, returns the
    /// generation token the capture task must check to know it is current.
    pub fn start(&self, interval_ms: u64, max_frames: usize, label: Option<String>) -> u64 {
        self.interval_ms
            .store(interval_ms.max(50), Ordering::Relaxed);
        self.max_frames
            .store(max_frames.clamp(1, 600), Ordering::Relaxed);
        {
            let mut f = self
                .frames
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            f.clear();
        }
        {
            let mut l = self
                .label
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *l = label;
        }
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.active.store(true, Ordering::SeqCst);
        generation
    }

    /// Stop the current trace. Returns the captured frame count.
    pub fn stop(&self) -> usize {
        self.active.store(false, Ordering::SeqCst);
        // Invalidate any running task.
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.frame_count()
    }

    /// Whether a trace is currently active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// The current generation token (a capture task is stale if it differs).
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
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
    /// inserting a frame into a trace started after it.
    pub fn push_frame_if_current(&self, generation: u64, t_ms: u64, data_b64: String) -> bool {
        if !self.is_active() || self.generation() != generation {
            return false;
        }
        self.push_frame(t_ms, data_b64);
        true
    }

    /// Record (or clear) the session id of the recording the current trace started.
    pub fn set_owned_recording(&self, session_id: Option<String>) {
        *self
            .owned_recording
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = session_id;
    }

    /// Clear and return the session id of the recording the finished trace started.
    pub fn take_owned_recording(&self) -> Option<String> {
        self.owned_recording
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Stop the trace only if `generation` is still current — atomically, so an old capture
    /// task's auto-stop can never stop a NEWER trace started in between. Returns whether it
    /// stopped anything.
    pub fn stop_if_generation(&self, generation: u64) -> bool {
        if self
            .generation
            .compare_exchange(
                generation,
                generation + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            self.active.store(false, Ordering::SeqCst);
            true
        } else {
            false
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
        let g1 = sc.start(200, 10, Some("main".into()));
        sc.push_frame(0, "x".into());
        assert_eq!(sc.frame_count(), 1);
        let g2 = sc.start(200, 10, None);
        assert!(g2 > g1, "generation must increase");
        assert_eq!(sc.frame_count(), 0, "start clears frames");
        assert!(sc.is_active());
    }

    #[test]
    fn stop_deactivates_and_invalidates() {
        let sc = Screencast::default();
        let g = sc.start(200, 10, None);
        sc.stop();
        assert!(!sc.is_active());
        assert!(sc.generation() > g, "stop invalidates the task generation");
    }

    #[test]
    fn stale_generation_push_is_rejected() {
        let sc = Screencast::default();
        let old = sc.start(100, 10, None);
        let new = sc.start(100, 10, None);
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
        sc.set_owned_recording(Some("s1".into()));
        assert_eq!(sc.take_owned_recording().as_deref(), Some("s1"));
        assert_eq!(sc.take_owned_recording(), None);
    }

    #[test]
    fn stale_generation_cannot_stop_a_newer_trace() {
        let sc = Screencast::default();
        let old = sc.start(100, 10, None);
        let new = sc.start(100, 10, None);
        assert!(
            !sc.stop_if_generation(old),
            "an old task must not stop the new trace"
        );
        assert!(sc.is_active());
        assert!(sc.stop_if_generation(new));
        assert!(!sc.is_active());
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
