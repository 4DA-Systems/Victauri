#![deny(missing_docs)]
//! Core types and protocol for Victauri — full-stack introspection for Tauri apps via MCP.
//!
//! This crate provides the shared type system used by all Victauri crates.
//! It has no Tauri dependency and can be used independently for testing.

#[doc(hidden)]
pub extern crate inventory;

pub mod codegen;
pub mod error;
pub mod event;
pub mod recording;
pub mod registry;
pub mod snapshot;
pub mod types;
pub mod verification;

pub use codegen::{CodegenOptions, generate_test, generate_test_default};
pub use error::VictauriError;
pub use event::{AppEvent, EventLog, InteractionKind, IpcCall, IpcResult};
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

/// Acquire a mutex lock, recovering from poisoning with a warning.
///
/// Victauri's mutex-protected data is append-only logs and registries where
/// stale data is preferable to crashing the testing framework.
pub fn acquire_lock<'a, T>(
    mutex: &'a std::sync::Mutex<T>,
    context: &str,
) -> std::sync::MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        tracing::error!("{context}: mutex was poisoned, recovering");
        poisoned.into_inner()
    })
}

/// Acquire a read lock on an `RwLock`, recovering from poisoning.
pub fn acquire_read<'a, T>(
    lock: &'a std::sync::RwLock<T>,
    context: &str,
) -> std::sync::RwLockReadGuard<'a, T> {
    lock.read().unwrap_or_else(|poisoned| {
        tracing::error!("{context}: RwLock was poisoned, recovering with read guard");
        poisoned.into_inner()
    })
}

/// Acquire a write lock on an `RwLock`, recovering from poisoning.
pub fn acquire_write<'a, T>(
    lock: &'a std::sync::RwLock<T>,
    context: &str,
) -> std::sync::RwLockWriteGuard<'a, T> {
    lock.write().unwrap_or_else(|poisoned| {
        tracing::error!("{context}: RwLock was poisoned, recovering with write guard");
        poisoned.into_inner()
    })
}
