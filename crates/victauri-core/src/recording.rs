//! Time-travel recording: captures event streams and state checkpoints
//! for replay and debugging.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::error::VictauriError;
use crate::event::{AppEvent, IpcCall};

const DEFAULT_MAX_CHECKPOINTS: usize = 1000;
const DEFAULT_MAX_EVENTS: usize = 50_000;

/// A snapshot of application state taken at a specific point during recording.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
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
pub struct RecordedEvent {
    /// Monotonically increasing sequence number within the recording session.
    pub index: usize,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// The captured application event.
    pub event: AppEvent,
}

/// Thread-safe session recorder for time-travel debugging. Records events and
/// state checkpoints during a recording session. Only one session can be active at a time.
#[derive(Debug, Clone)]
pub struct EventRecorder {
    recording: Arc<Mutex<Option<ActiveRecording>>>,
    max_events: usize,
}

#[derive(Debug, Clone)]
struct ActiveRecording {
    session_id: String,
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
            max_events,
        }
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
        let mut rec = crate::acquire_lock(&self.recording, "EventRecorder");
        if rec.is_some() {
            return Err(VictauriError::RecordingAlreadyActive);
        }
        *rec = Some(ActiveRecording {
            session_id,
            started_at: Utc::now(),
            events: VecDeque::new(),
            checkpoints: VecDeque::new(),
            event_counter: 0,
            max_events: self.max_events,
            max_checkpoints: DEFAULT_MAX_CHECKPOINTS,
        });
        Ok(())
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
        rec.take().map(|r| RecordedSession {
            id: r.session_id,
            started_at: r.started_at,
            events: r.events.into_iter().collect(),
            checkpoints: r.checkpoints.into_iter().collect(),
        })
    }

    /// Returns true if a recording session is currently active.
    #[must_use]
    pub fn is_recording(&self) -> bool {
        crate::acquire_lock(&self.recording, "EventRecorder")
            .is_some()
    }

    /// Appends an event to the active recording, evicting the oldest if at capacity.
    pub fn record_event(&self, event: AppEvent) {
        let mut rec = crate::acquire_lock(&self.recording, "EventRecorder");
        if let Some(ref mut active) = *rec {
            let timestamp = event.timestamp();
            let index = active.event_counter;
            active.event_counter += 1;

            if active.events.len() >= active.max_events {
                active.events.pop_front();
            }

            active.events.push_back(RecordedEvent {
                index,
                timestamp,
                event,
            });
        }
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
    #[must_use]
    pub fn events_since(&self, index: usize) -> Vec<RecordedEvent> {
        let rec = crate::acquire_lock(&self.recording, "EventRecorder");
        match rec.as_ref() {
            Some(active) => active
                .events
                .iter()
                .filter(|e| e.index >= index)
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    }

    /// Returns events whose timestamps fall within the given inclusive range.
    #[must_use]
    pub fn events_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<RecordedEvent> {
        let rec = crate::acquire_lock(&self.recording, "EventRecorder");
        match rec.as_ref() {
            Some(active) => active
                .events
                .iter()
                .filter(|e| e.timestamp >= from && e.timestamp <= to)
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    }

    /// Returns all checkpoints from the active recording session.
    #[must_use]
    pub fn get_checkpoints(&self) -> Vec<StateCheckpoint> {
        let rec = crate::acquire_lock(&self.recording, "EventRecorder");
        match rec.as_ref() {
            Some(active) => active.checkpoints.iter().cloned().collect(),
            None => Vec::new(),
        }
    }

    /// Returns events recorded between two named checkpoints.
    ///
    /// # Errors
    ///
    /// - [`VictauriError::NoActiveRecording`] if no session is active.
    /// - [`VictauriError::CheckpointNotFound`] if either checkpoint ID does not exist.
    pub fn events_between_checkpoints(
        &self,
        from_checkpoint_id: &str,
        to_checkpoint_id: &str,
    ) -> crate::error::Result<Vec<RecordedEvent>> {
        let rec = crate::acquire_lock(&self.recording, "EventRecorder");
        let active = rec.as_ref().ok_or(VictauriError::NoActiveRecording)?;

        let from_idx = active
            .checkpoints
            .iter()
            .find(|c| c.id == from_checkpoint_id)
            .ok_or_else(|| VictauriError::CheckpointNotFound {
                id: from_checkpoint_id.to_string(),
            })?
            .event_index;
        let to_idx = active
            .checkpoints
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

        Ok(active
            .events
            .iter()
            .filter(|e| e.index >= start && e.index < end)
            .cloned()
            .collect())
    }

    /// Snapshot the current recording as a session WITHOUT stopping it.
    #[must_use]
    pub fn export(&self) -> Option<RecordedSession> {
        let rec = crate::acquire_lock(&self.recording, "EventRecorder");
        rec.as_ref().map(|r| RecordedSession {
            id: r.session_id.clone(),
            started_at: r.started_at,
            events: r.events.iter().cloned().collect(),
            checkpoints: r.checkpoints.iter().cloned().collect(),
        })
    }

    /// Import a previously exported session, replacing any active recording.
    pub fn import(&self, session: RecordedSession) {
        let event_counter = session.events.last().map_or(0, |e| e.index + 1);
        let max_events = self.max_events;
        let mut rec = crate::acquire_lock(&self.recording, "EventRecorder");
        *rec = Some(ActiveRecording {
            session_id: session.id,
            started_at: session.started_at,
            events: session.events.into_iter().collect(),
            checkpoints: session.checkpoints.into_iter().collect(),
            event_counter,
            max_events,
            max_checkpoints: DEFAULT_MAX_CHECKPOINTS,
        });
    }

    /// Extracts IPC calls in order from the recording for replay.
    #[must_use]
    pub fn ipc_replay_sequence(&self) -> Vec<IpcCall> {
        let rec = crate::acquire_lock(&self.recording, "EventRecorder");
        match rec.as_ref() {
            Some(active) => active
                .events
                .iter()
                .filter_map(|re| match &re.event {
                    AppEvent::Ipc(call) => Some(call.clone()),
                    _ => None,
                })
                .collect(),
            None => Vec::new(),
        }
    }
}

impl Default for EventRecorder {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_EVENTS)
    }
}
