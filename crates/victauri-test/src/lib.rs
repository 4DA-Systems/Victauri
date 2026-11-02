#![deny(missing_docs)]
// `deny`, not `forbid`: the one audited exception is the Win32 liveness query in `process`.
#![deny(unsafe_code)]
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
//!     assert_eq!(audit["violations"], serde_json::json!([]));
//! }
//! ```

mod app;
mod assertions;
mod client;
pub mod coverage;
mod discovery;
mod error;
pub mod locator;
pub mod prelude;
#[doc(hidden)]
pub mod process;
pub mod reporting;
pub mod smoke;
#[doc(hidden)]
pub mod terminal;
pub mod visual;

pub use app::TestApp;
pub use assertions::{
    CheckResult, VerifyBuilder, VerifyReport, assert_ipc_called, assert_ipc_called_with,
    assert_ipc_not_called,
};
#[doc(hidden)]
pub use client::health_status_means_alive;
pub use client::{
    MemoryStats, PluginInfo, PluginToolInfo, VictauriClient, WaitForBuilder, assert_ipc_healthy,
    assert_json_eq, assert_json_truthy, assert_no_a11y_violations, assert_performance_budget,
    assert_state_matches,
};
pub use discovery::real_port_claims;
pub use error::TestError;
pub use locator::{Bounds, Locator, LocatorExpect, LocatorMatch};
pub use smoke::{SmokeCheckResult, SmokeConfig, SmokeReport};

/// Returns `true` if E2E tests should run: `VICTAURI_E2E` is set to anything but an empty value,
/// `0`, `false` or `off`.
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
    e2e_flag_is_on(std::env::var("VICTAURI_E2E").ok().as_deref())
}

fn e2e_flag_is_on(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        let value = value.trim();
        !(value.is_empty()
            || value == "0"
            || value.eq_ignore_ascii_case("false")
            || value.eq_ignore_ascii_case("off"))
    })
}

/// What [`e2e_test!`] does when E2E tests are off.
///
/// Such a test still reports as passed, having asserted nothing, because a stable libtest test
/// cannot skip itself. So it says so: written to stderr directly, since `eprintln!` output of a
/// passing test is swallowed by libtest's capture.
#[doc(hidden)]
pub fn e2e_skipped(test: &str) {
    use std::io::Write as _;
    let _ = writeln!(
        std::io::stderr(),
        "SKIPPED {test}: nothing was tested, because VICTAURI_E2E is not on \
         (set VICTAURI_E2E=1 with the app running)"
    );
}

/// Connect to a Victauri server using standard env var configuration.
///
/// Reads `VICTAURI_PORT` and `VICTAURI_AUTH_TOKEN` (both optional), else discovers the running app.
/// This is a shorthand for `VictauriClient::discover()`.
///
/// # Errors
///
/// Returns [`TestError::Other`] if no single app can be picked, and [`TestError::Connection`] if the
/// server is unreachable.
pub async fn connect() -> Result<VictauriClient, TestError> {
    VictauriClient::discover().await
}

/// Declare an E2E test that skips when `VICTAURI_E2E` is not on (see [`is_e2e`]) and auto-connects
/// to the running server.
///
/// A skipped test prints a `SKIPPED` line and reports as passed; it does not look for a server.
/// With `VICTAURI_E2E` on and no app running, the test fails.
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
                $crate::e2e_skipped(stringify!($name));
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
                $crate::e2e_skipped(stringify!($name));
                return;
            }
            let mut $client = $crate::connect().await.unwrap();
            $body
        }
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn e2e_is_off_unless_the_flag_says_on() {
        for off in [
            None,
            Some(""),
            Some(" "),
            Some("0"),
            Some("false"),
            Some("OFF"),
        ] {
            assert!(!super::e2e_flag_is_on(off), "{off:?}");
        }
        for on in [Some("1"), Some("true"), Some("yes")] {
            assert!(super::e2e_flag_is_on(on), "{on:?}");
        }
    }
}
