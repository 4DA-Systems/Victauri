#![deny(missing_docs)]
//! Core types and protocol for Victauri — full-stack introspection for Tauri apps via MCP.
//!
//! This crate provides the shared type system used by all Victauri crates.
//! It has no Tauri dependency and can be used independently for testing.

#[doc(hidden)]
pub extern crate inventory;

pub mod error;
pub mod event;
pub mod recording;
pub mod registry;
pub mod snapshot;
pub mod types;
pub mod verification;

pub use error::VictauriError;
pub use event::{AppEvent, EventLog, IpcCall, IpcResult};
pub use recording::{EventRecorder, RecordedEvent, RecordedSession, StateCheckpoint};
pub use registry::{
    CommandArg, CommandInfo, CommandInfoFactory, CommandRegistry, ScoredCommand,
    auto_discovered_commands,
};
pub use snapshot::{DomElement, DomSnapshot, WindowState};
pub use types::{Divergence, DivergenceSeverity, MemoryDelta, RefHandle, VerificationResult};
pub use verification::{
    AssertionCondition, AssertionResult, GhostCommand, GhostCommandReport, GhostSource,
    IpcIntegrityReport, SemanticAssertion, check_ipc_integrity, detect_ghost_commands,
    evaluate_assertion, verify_state,
};
