//! Time-travel recording: captures event streams and state checkpoints
//! for replay and debugging.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::VictauriError;
use crate::event::{AppEvent, IpcCall};

const DEFAULT_MAX_CHECKPOINTS: usize = 1000;
const DEFAULT_MAX_EVENTS: usize = 50_000;

/// A snapshot of application state taken at a specific point during recording.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct StateCheckpoint {
    /// Unique identifier for this checkpoint.
    pub id: String,
    /// Optional human-readable label for the checkpoint.
    pub label: Option<String>,
    /// When the checkpoint was created.
    pub timestamp: DateTime<Utc>,
    /// Serialized application state at the checkpoint.
    pub state: serde_json::Value,
    /// Index into the event stream at the time of this checkpoint.
    pub event_index: usize,
}

/// A complete recorded session with events and state checkpoints. Serializable for export/import.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RecordedSession {
    /// Unique session identifier (UUID).
    pub id: String,
    /// When the recording session began.
    pub started_at: DateTime<Utc>,
    /// All events captured during the session, in order.
    pub events: Vec<RecordedEvent>,
    /// State checkpoints created during the session.
    pub checkpoints: Vec<StateCheckpoint>,
}

/// A single event captured during a recording session, with its sequence index.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RecordedEvent {
    /// Monotonically increasing sequence number within the recording session.
    pub index: usize,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// The captured application event.
    pub event: AppEvent,
}

impl RecordedSession {
    /// Creates a recorded session from its parts (e.g. when importing a
    /// session built outside a live [`EventRecorder`]).
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        started_at: DateTime<Utc>,
        events: Vec<RecordedEvent>,
        checkpoints: Vec<StateCheckpoint>,
    ) -> Self {
        Self {
            id: id.into(),
            started_at,
            events,
            checkpoints,
        }
    }
}

impl RecordedEvent {
    /// Creates a recorded event with the given sequence index.
    ///
    /// # Examples
    ///
    /// ```
    /// use victauri_core::{AppEvent, RecordedEvent, RecordedSession};
    ///
    /// let now = chrono::Utc::now();
    /// let ev = RecordedEvent::new(0, now, AppEvent::window_event("main", "focus", now));
    /// let session = RecordedSession::new("s1", now, vec![ev], vec![]);
    /// assert_eq!(session.events.len(), 1);
    /// ```
    #[must_use]
    pub fn new(index: usize, timestamp: DateTime<Utc>, event: AppEvent) -> Self {
        Self {
            index,
            timestamp,
            event,
        }
    }
}

/// Thread-safe session recorder for time-travel debugging. Records events and
/// state checkpoints during a recording session. Only one session can be active at a time.
#[derive(Debug, Clone)]
pub struct EventRecorder {
    recording: Arc<Mutex<Option<ActiveRecording>>>,
    last_session: Arc<Mutex<Option<RecordedSession>>>,
    max_events: usize,
    /// Source of recording generations: every `start`/`import` takes a fresh one, so a
    /// caller holding an old generation can never touch a later recording — even one that
    /// reuses the same (caller-chosen) session id.
    generations: Arc<AtomicU64>,
}

#[derive(Debug, Clone)]
struct ActiveRecording {
    session_id: String,
    generation: u64,
    started_at: DateTime<Utc>,
    events: VecDeque<RecordedEvent>,
    checkpoints: VecDeque<StateCheckpoint>,
    event_counter: usize,
    max_events: usize,
    max_checkpoints: usize,
}

impl EventRecorder {
    /// Creates a new recorder with the given maximum event capacity.
    ///
    /// ```
    /// use victauri_core::EventRecorder;
    ///
    /// let recorder = EventRecorder::new(1000);
    /// assert!(!recorder.is_recording());
    /// assert_eq!(recorder.event_count(), 0);
    /// ```
    #[must_use]
    pub fn new(max_events: usize) -> Self {
        Self {
            recording: Arc::new(Mutex::new(None)),
            last_session: Arc::new(Mutex::new(None)),
            max_events,
            generations: Arc::new(AtomicU64::new(0)),
        }
    }

    fn next_generation(&self) -> u64 {
        self.generations.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Starts a new recording session; returns `Err` if one is already active.
    ///
    /// # Errors
    ///
    /// Returns [`VictauriError::RecordingAlreadyActive`] if a session is already in progress.
    ///
    /// # Examples
    ///
    /// ```
    /// use victauri_core::EventRecorder;
    ///
    /// let recorder = EventRecorder::new(1000);
    /// recorder.start("session-1".to_string()).unwrap();
    /// assert!(recorder.is_recording());
    /// ```
    pub fn start(&self, session_id: String) -> crate::error::Result<()> {
        self.start_session(session_id).map(|_| ())
    }

    /// [`start`](Self::start), returning the new recording's generation — the token for
    /// [`record_event_if`](Self::record_event_if) and [`stop_if_generation`](Self::stop_if_generation).
    ///
    /// # Errors
    ///
    /// Returns [`VictauriError::RecordingAlreadyActive`] if a session is already in progress.
    pub fn start_session(&self, session_id: String) -> crate::error::Result<u64> {
        let mut rec = crate::acquire_lock(&self.recording, "EventRecorder");
        if rec.is_some() {
            return Err(VictauriError::RecordingAlreadyActive);
        }
        let generation = self.next_generation();
        *rec = Some(ActiveRecording {
            session_id,
            generation,
            started_at: Utc::now(),
            events: VecDeque::new(),
            checkpoints: VecDeque::new(),
            event_counter: 0,
            max_events: self.max_events,
            max_checkpoints: DEFAULT_MAX_CHECKPOINTS,
        });
        Ok(generation)
    }

    /// Stops the active recording and returns the completed session, or None if not recording.
    ///
    /// # Examples
    ///
    /// ```
    /// use victauri_core::EventRecorder;
    ///
    /// let recorder = EventRecorder::new(1000);
    /// recorder.start("session-1".to_string()).unwrap();
    /// let session = recorder.stop().expect("should return session");
    /// assert_eq!(session.id, "session-1");
    /// assert!(!recorder.is_recording());
    /// ```
    #[must_use]
    pub fn stop(&self) -> Option<RecordedSession> {
        let mut rec = crate::acquire_lock(&self.recording, "EventRecorder");
        self.finish(&mut rec)
    }

    /// Take the active recording out of `rec` (whose lock the caller holds) and keep it as the
    /// last session.
    fn finish(&self, rec: &mut Option<ActiveRecording>) -> Option<RecordedSession> {
        rec.take().map(|r| {
            let session = RecordedSession {
                id: r.session_id,
                started_at: r.started_at,
                events: r.events.into_iter().collect(),
                checkpoints: r.checkpoints.into_iter().collect(),
            };
            *crate::acquire_lock(&self.last_session, "EventRecorder::last_session") =
                Some(session.clone());
            session
        })
    }

    /// Returns true if a recording session is currently active.
    #[must_use]
    pub fn is_recording(&self) -> bool {
        crate::acquire_lock(&self.recording, "EventRecorder").is_some()
    }

    /// Generation of the active recording, or `None` if not recording.
    #[must_use]
    pub fn generation(&self) -> Option<u64> {
        crate::acquire_lock(&self.recording, "EventRecorder")
            .as_ref()
            .map(|r| r.generation)
    }

    /// Appends an event to the active recording, evicting the oldest if at capacity.
    pub fn record_event(&self, event: AppEvent) {
        let mut rec = crate::acquire_lock(&self.recording, "EventRecorder");
        if let Some(ref mut active) = *rec {
            Self::append(active, event);
        }
    }

    /// [`record_event`](Self::record_event), but only into the recording of `generation`.
    /// A reader that captured the generation before a slow read (the webview drain) must not
    /// append what it read into a recording started after it — those events predate it.
    /// Returns whether the event was recorded.
    #[must_use]
    pub fn record_event_if(&self, generation: u64, event: AppEvent) -> bool {
        let mut rec = crate::acquire_lock(&self.recording, "EventRecorder");
        match rec.as_mut() {
            Some(active) if active.generation == generation => {
                Self::append(active, event);
                true
            }
            _ => false,
        }
    }

    fn append(active: &mut ActiveRecording, event: AppEvent) {
        let timestamp = event.timestamp();
        let index = active.event_counter;
        // Saturating: an imported session can seed event_counter at usize::MAX
        // (its index is attacker-controlled); a bare `+= 1` would then panic in
        // debug / wrap in release on the next auto-captured event (audit #18).
        active.event_counter = active.event_counter.saturating_add(1);

        if active.events.len() >= active.max_events {
            active.events.pop_front();
        }

        active.events.push_back(RecordedEvent {
            index,
            timestamp,
            event,
        });
    }

    /// Creates a named state checkpoint at the current event index; returns `Err` if not recording.
    ///
    /// # Errors
    ///
    /// Returns [`VictauriError::NoActiveRecording`] if no session is in progress.
    pub fn checkpoint(
        &self,
        id: String,
        label: Option<String>,
        state: serde_json::Value,
    ) -> crate::error::Result<()> {
        let mut rec = crate::acquire_lock(&self.recording, "EventRecorder");
        if let Some(ref mut active) = *rec {
            let event_index = active.event_counter;
            if active.checkpoints.len() >= active.max_checkpoints {
                active.checkpoints.pop_front();
            }
            active.checkpoints.push_back(StateCheckpoint {
                id,
                label,
                timestamp: Utc::now(),
                state,
                event_index,
            });
            Ok(())
        } else {
            Err(VictauriError::NoActiveRecording)
        }
    }

    /// Session id of the active recording, or `None` if not recording.
    #[must_use]
    pub fn active_session_id(&self) -> Option<String> {
        crate::acquire_lock(&self.recording, "EventRecorder")
            .as_ref()
            .map(|r| r.session_id.clone())
    }

    /// Stop the active recording only if it is the session `session_id` (a caller that started
    /// a recording must not stop a different one someone else started after it ended).
    ///
    /// The id check and the stop happen under one lock: checked and stopped separately, a
    /// stop + start by someone else in between made this stop the OTHER recording. Session ids
    /// are caller-chosen and may repeat, so an owner that must never stop a later recording
    /// with the same id should hold its generation and use
    /// [`stop_if_generation`](Self::stop_if_generation).
    #[must_use]
    pub fn stop_if_session(&self, session_id: &str) -> Option<RecordedSession> {
        let mut rec = crate::acquire_lock(&self.recording, "EventRecorder");
        if rec.as_ref().is_some_and(|r| r.session_id == session_id) {
            self.finish(&mut rec)
        } else {
            None
        }
    }

    /// Stop the active recording only if it is the one of `generation` (atomically).
    #[must_use]
    pub fn stop_if_generation(&self, generation: u64) -> Option<RecordedSession> {
        let mut rec = crate::acquire_lock(&self.recording, "EventRecorder");
        if rec.as_ref().is_some_and(|r| r.generation == generation) {
            self.finish(&mut rec)
        } else {
            None
        }
    }

    /// When the active recording started, or `None` if not recording.
    #[must_use]
    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        crate::acquire_lock(&self.recording, "EventRecorder")
            .as_ref()
            .map(|r| r.started_at)
    }

    /// Timestamp of the newest event in the active recording (or its start time if it has no
    /// events yet); `None` if not recording. A caller pulling more events can read strictly
    /// after this to avoid re-recording what is already there.
    #[must_use]
    pub fn latest_event_timestamp(&self) -> Option<DateTime<Utc>> {
        crate::acquire_lock(&self.recording, "EventRecorder")
            .as_ref()
            .map(|r| {
                r.events
                    .iter()
                    .map(|e| e.timestamp)
                    .max()
                    .unwrap_or(r.started_at)
            })
    }

    /// Returns the number of events recorded so far, or 0 if not recording.
    #[must_use]
    pub fn event_count(&self) -> usize {
        crate::acquire_lock(&self.recording, "EventRecorder")
            .as_ref()
            .map_or(0, |r| r.events.len())
    }

    /// Returns the number of checkpoints created so far, or 0 if not recording.
    #[must_use]
    pub fn checkpoint_count(&self) -> usize {
        crate::acquire_lock(&self.recording, "EventRecorder")
            .as_ref()
            .map_or(0, |r| r.checkpoints.len())
    }

    /// Returns all events with an index >= the given value.
    /// Falls back to the last stopped session if no active recording.
    #[must_use]
    pub fn events_since(&self, index: usize) -> Vec<RecordedEvent> {
        let rec = crate::acquire_lock(&self.recording, "EventRecorder");
        if let Some(active) = rec.as_ref() {
            return active
                .events
                .iter()
                .filter(|e| e.index >= index)
                .cloned()
                .collect();
        }
        drop(rec);
        let last = crate::acquire_lock(&self.last_session, "EventRecorder::last_session");
        last.as_ref().map_or_else(Vec::new, |session| {
            session
                .events
                .iter()
                .filter(|e| e.index >= index)
                .cloned()
                .collect()
        })
    }

    /// Returns events whose timestamps fall within the given inclusive range.
    /// Falls back to the last stopped session if no active recording.
    #[must_use]
    pub fn events_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<RecordedEvent> {
        let rec = crate::acquire_lock(&self.recording, "EventRecorder");
        if let Some(active) = rec.as_ref() {
            return active
                .events
                .iter()
                .filter(|e| e.timestamp >= from && e.timestamp <= to)
                .cloned()
                .collect();
        }
        drop(rec);
        let last = crate::acquire_lock(&self.last_session, "EventRecorder::last_session");
        last.as_ref().map_or_else(Vec::new, |session| {
            session
                .events
                .iter()
                .filter(|e| e.timestamp >= from && e.timestamp <= to)
                .cloned()
                .collect()
        })
    }

    /// Returns all checkpoints from the active recording session.
    /// Falls back to the last stopped session if no active recording.
    #[must_use]
    pub fn get_checkpoints(&self) -> Vec<StateCheckpoint> {
        let rec = crate::acquire_lock(&self.recording, "EventRecorder");
        if let Some(active) = rec.as_ref() {
            return active.checkpoints.iter().cloned().collect();
        }
        drop(rec);
        let last = crate::acquire_lock(&self.last_session, "EventRecorder::last_session");
        last.as_ref()
            .map_or_else(Vec::new, |session| session.checkpoints.to_vec())
    }

    /// Returns events recorded between two named checkpoints.
    /// Falls back to the last stopped session if no active recording.
    ///
    /// # Errors
    ///
    /// - [`VictauriError::NoActiveRecording`] if no session is active and no last session exists.
    /// - [`VictauriError::CheckpointNotFound`] if either checkpoint ID does not exist.
    pub fn events_between_checkpoints(
        &self,
        from_checkpoint_id: &str,
        to_checkpoint_id: &str,
    ) -> crate::error::Result<Vec<RecordedEvent>> {
        let rec = crate::acquire_lock(&self.recording, "EventRecorder");
        let source_checkpoints;
        let source_events;
        if let Some(active) = rec.as_ref() {
            source_checkpoints = active.checkpoints.iter().cloned().collect::<Vec<_>>();
            source_events = active.events.iter().cloned().collect::<Vec<_>>();
        } else {
            drop(rec);
            let last = crate::acquire_lock(&self.last_session, "EventRecorder::last_session");
            let session = last.as_ref().ok_or(VictauriError::NoActiveRecording)?;
            source_checkpoints = session.checkpoints.clone();
            source_events = session.events.clone();
        }

        let from_idx = source_checkpoints
            .iter()
            .find(|c| c.id == from_checkpoint_id)
            .ok_or_else(|| VictauriError::CheckpointNotFound {
                id: from_checkpoint_id.to_string(),
            })?
            .event_index;
        let to_idx = source_checkpoints
            .iter()
            .find(|c| c.id == to_checkpoint_id)
            .ok_or_else(|| VictauriError::CheckpointNotFound {
                id: to_checkpoint_id.to_string(),
            })?
            .event_index;

        let (start, end) = if from_idx <= to_idx {
            (from_idx, to_idx)
        } else {
            (to_idx, from_idx)
        };

        Ok(source_events
            .iter()
            .filter(|e| e.index >= start && e.index < end)
            .cloned()
            .collect())
    }

    /// Snapshot the current recording as a session WITHOUT stopping it.
    /// Falls back to the last stopped session if no active recording.
    #[must_use]
    pub fn export(&self) -> Option<RecordedSession> {
        let rec = crate::acquire_lock(&self.recording, "EventRecorder");
        if let Some(r) = rec.as_ref() {
            return Some(RecordedSession {
                id: r.session_id.clone(),
                started_at: r.started_at,
                events: r.events.iter().cloned().collect(),
                checkpoints: r.checkpoints.iter().cloned().collect(),
            });
        }
        drop(rec);
        crate::acquire_lock(&self.last_session, "EventRecorder::last_session").clone()
    }

    /// Import a previously exported session, replacing any active recording.
    ///
    /// The imported session is fully caller-controlled, so its collections are
    /// clamped to the configured caps (keeping the most recent entries) before
    /// becoming active — an oversized session cannot inflate resident memory past
    /// `max_events`/`max_checkpoints` (audit #19), and a crafted event index of
    /// `usize::MAX` cannot overflow the counter (audit #18).
    pub fn import(&self, session: RecordedSession) {
        let active = self.imported(session);
        *crate::acquire_lock(&self.recording, "EventRecorder") = Some(active);
    }

    /// [`import`](Self::import), but only if no recording is active — checked and replaced under
    /// one lock, so a recording started concurrently is never silently discarded. Returns the
    /// imported recording's generation.
    ///
    /// # Errors
    ///
    /// Returns [`VictauriError::RecordingAlreadyActive`] if a session is in progress.
    pub fn import_if_idle(&self, session: RecordedSession) -> crate::error::Result<u64> {
        let active = self.imported(session);
        let generation = active.generation;
        let mut rec = crate::acquire_lock(&self.recording, "EventRecorder");
        if rec.is_some() {
            return Err(VictauriError::RecordingAlreadyActive);
        }
        *rec = Some(active);
        Ok(generation)
    }

    fn imported(&self, session: RecordedSession) -> ActiveRecording {
        let max_events = self.max_events;
        let max_checkpoints = DEFAULT_MAX_CHECKPOINTS;

        let mut events: std::collections::VecDeque<RecordedEvent> =
            session.events.into_iter().collect();
        while events.len() > max_events {
            events.pop_front();
        }
        let mut checkpoints: std::collections::VecDeque<StateCheckpoint> =
            session.checkpoints.into_iter().collect();
        while checkpoints.len() > max_checkpoints {
            checkpoints.pop_front();
        }

        let event_counter = events.back().map_or(0, |e| e.index.saturating_add(1));

        ActiveRecording {
            session_id: session.id,
            generation: self.next_generation(),
            started_at: session.started_at,
            events,
            checkpoints,
            event_counter,
            max_events,
            max_checkpoints,
        }
    }

    /// Extracts IPC calls in order from the active recording or last stopped session for replay.
    #[must_use]
    pub fn ipc_replay_sequence(&self) -> Vec<IpcCall> {
        let rec = crate::acquire_lock(&self.recording, "EventRecorder");
        if let Some(active) = rec.as_ref() {
            return active
                .events
                .iter()
                .filter_map(|re| match &re.event {
                    AppEvent::Ipc(call) => Some(call.clone()),
                    _ => None,
                })
                .collect();
        }
        drop(rec);
        let last = crate::acquire_lock(&self.last_session, "EventRecorder::last_session");
        last.as_ref().map_or_else(Vec::new, |session| {
            session
                .events
                .iter()
                .filter_map(|re| match &re.event {
                    AppEvent::Ipc(call) => Some(call.clone()),
                    _ => None,
                })
                .collect()
        })
    }
}

impl Default for EventRecorder {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_EVENTS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn console(msg: &str) -> AppEvent {
        AppEvent::console("log".to_string(), msg.to_string(), Utc::now())
    }

    // C15a: the id check and the stop must be one step. With the check and the stop under
    // separate locks, a stop+start by someone else in between made `stop_if_session("x")`
    // stop (and return) the OTHER recording.
    #[test]
    fn stop_if_session_never_stops_a_different_session() {
        let rec = EventRecorder::new(100);
        let racer = rec.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let t = std::thread::spawn(move || {
            while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = racer.stop();
                let _ = racer.start("other".to_string());
            }
        });
        let mut wrong = 0;
        for _ in 0..200_000 {
            let _ = rec.start("mine".to_string());
            if let Some(s) = rec.stop_if_session("mine")
                && s.id != "mine"
            {
                wrong += 1;
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        t.join().unwrap();
        assert_eq!(wrong, 0, "stop_if_session stopped someone else's recording");
    }

    // C15a (ABA): session ids are caller-chosen and may repeat, so ownership is by generation.
    #[test]
    fn stale_generation_cannot_stop_a_later_recording_with_the_same_id() {
        let rec = EventRecorder::new(100);
        let old = rec.start_session("s".to_string()).unwrap();
        let _ = rec.stop();
        let new = rec.start_session("s".to_string()).unwrap();
        assert_ne!(old, new);
        assert!(rec.stop_if_generation(old).is_none());
        assert!(rec.is_recording());
        assert_eq!(rec.stop_if_generation(new).unwrap().id, "s");
    }

    // C6: a reader holding an old generation must not append into a newer recording.
    #[test]
    fn record_event_if_rejects_a_stale_generation() {
        let rec = EventRecorder::new(100);
        let a = rec.start_session("A".to_string()).unwrap();
        assert!(rec.record_event_if(a, console("during A")));
        let _ = rec.stop();
        let b = rec.start_session("B".to_string()).unwrap();
        assert!(!rec.record_event_if(a, console("late A read")));
        assert!(rec.record_event_if(b, console("during B")));
        assert_eq!(rec.event_count(), 1);
        assert_eq!(rec.generation(), Some(b));
    }

    // C15b: import must not replace a recording that became active after an is_recording check.
    #[test]
    fn import_if_idle_refuses_while_recording() {
        let rec = EventRecorder::new(100);
        rec.start("live".to_string()).unwrap();
        let session = RecordedSession::new("imported", Utc::now(), vec![], vec![]);
        assert!(rec.import_if_idle(session.clone()).is_err());
        assert_eq!(rec.active_session_id().as_deref(), Some("live"));
        let _ = rec.stop();
        let g = rec.import_if_idle(session).unwrap();
        assert_eq!(rec.generation(), Some(g));
        assert_eq!(rec.active_session_id().as_deref(), Some("imported"));
    }
}
