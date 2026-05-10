//! Convenience re-exports for concise test imports.
//!
//! ```rust,ignore
//! use victauri_test::prelude::*;
//!
//! #[tokio::test]
//! async fn my_test() {
//!     let mut client = VictauriClient::discover().await.unwrap();
//!     client.click_by_text("Submit").await.unwrap();
//!     client.expect_text("Success").await.unwrap();
//!
//!     let report = client.verify()
//!         .has_text("Success")
//!         .ipc_was_called("submit_form")
//!         .no_console_errors()
//!         .run().await.unwrap();
//!     report.assert_all_passed();
//! }
//! ```

pub use crate::app::TestApp;
pub use crate::assertions::{CheckResult, VerifyBuilder, VerifyReport};
pub use crate::client::VictauriClient;
pub use crate::error::TestError;
pub use crate::smoke::{SmokeCheckResult, SmokeConfig, SmokeReport};
pub use crate::visual::{MaskRegion, ThresholdPreset, VisualDiff, VisualOptions};
pub use crate::{
    assert_ipc_called, assert_ipc_called_with, assert_ipc_not_called, assert_json_eq,
    assert_json_truthy, assert_no_a11y_violations, assert_performance_budget, assert_state_matches,
    connect, is_e2e,
};
pub use serde_json::{Value, json};
