#![deny(missing_docs)]
//! Integration testing for Tauri apps via the Victauri MCP server.
//!
//! Provides [`TestApp`] for managed app lifecycle and [`VictauriClient`] with
//! Playwright-style interactions (`click_by_text`, `fill_by_id`, `expect_text`)
//! plus assertion helpers for accessibility, performance, and state verification.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use victauri_test::TestApp;
//!
//! #[tokio::test]
//! async fn greet_flow() {
//!     let app = TestApp::spawn("cargo run -p my-app").await.unwrap();
//!     let mut client = app.client().await.unwrap();
//!
//!     client.fill_by_id("name-input", "World").await.unwrap();
//!     client.click_by_id("greet-btn").await.unwrap();
//!     client.expect_text("Hello, World!").await.unwrap();
//! }
//! ```
//!
//! # Without `TestApp` (connect to existing server)
//!
//! ```rust,ignore
//! use victauri_test::VictauriClient;
//!
//! #[tokio::test]
//! async fn check_health() {
//!     let mut client = VictauriClient::discover().await.unwrap();
//!     let audit = client.audit_accessibility().await.unwrap();
//!     assert_eq!(audit["summary"]["violations"], 0);
//! }
//! ```

mod app;
mod assertions;
mod client;
pub mod coverage;
mod discovery;
mod error;
pub mod prelude;
pub mod reporting;
pub mod smoke;
pub mod visual;

pub use app::TestApp;
pub use assertions::{
    CheckResult, VerifyBuilder, VerifyReport, assert_ipc_called, assert_ipc_called_with,
    assert_ipc_not_called,
};
pub use client::{
    MemoryStats, PluginInfo, VictauriClient, WaitForBuilder, assert_ipc_healthy, assert_json_eq,
    assert_json_truthy, assert_no_a11y_violations, assert_performance_budget,
    assert_state_matches,
};
pub use error::TestError;
pub use smoke::{SmokeCheckResult, SmokeConfig, SmokeReport};

/// Returns `true` if E2E tests should run (i.e., `VICTAURI_E2E` env var is set).
///
/// Use this to gate integration tests that require a running Tauri app:
///
/// ```rust,ignore
/// #[tokio::test]
/// async fn my_test() {
///     if !victauri_test::is_e2e() { return; }
///     // ...
/// }
/// ```
#[must_use]
pub fn is_e2e() -> bool {
    std::env::var("VICTAURI_E2E").is_ok()
}

/// Connect to a Victauri server using standard env var configuration.
///
/// Reads `VICTAURI_PORT` (default 7373) and `VICTAURI_AUTH_TOKEN` (optional).
/// This is a shorthand for `VictauriClient::discover()`.
///
/// # Errors
///
/// Returns [`TestError::Connection`] if the server is unreachable.
pub async fn connect() -> Result<VictauriClient, TestError> {
    VictauriClient::discover().await
}

/// Declare an E2E test that auto-skips when `VICTAURI_E2E` is not set
/// and auto-connects to the running server.
///
/// # Example
///
/// ```rust,ignore
/// use victauri_test::{e2e_test, VictauriClient};
///
/// e2e_test!(greet_flow, |client: &mut VictauriClient| async move {
///     client.fill_by_id("name-input", "World").await.unwrap();
///     client.click_by_id("greet-btn").await.unwrap();
///     client.expect_text("Hello, World!").await.unwrap();
/// });
/// ```
#[macro_export]
macro_rules! e2e_test {
    ($name:ident, |$client:ident : &mut VictauriClient| async move $body:block) => {
        #[tokio::test]
        async fn $name() {
            if !$crate::is_e2e() {
                return;
            }
            let mut $client = $crate::connect().await.unwrap();
            $body
        }
    };
    ($name:ident, |$client:ident| async move $body:block) => {
        #[tokio::test]
        async fn $name() {
            if !$crate::is_e2e() {
                return;
            }
            let mut $client = $crate::connect().await.unwrap();
            $body
        }
    };
}
