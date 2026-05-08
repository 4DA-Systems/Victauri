# victauri-test

Playwright-style testing for Tauri apps via [Victauri](https://github.com/runyourempire/victauri).

## What It Does

Typed HTTP client for the Victauri MCP server with high-level convenience methods:

- **Playwright-style API** — `click_by_text`, `fill_by_id`, `expect_text`, `select_by_id`
- **IPC verification** — call logs, checkpoints, ghost command detection, coverage tracking
- **Visual regression** — pixel-level screenshot comparison with baseline snapshots
- **Fluent assertions** — chain DOM, IPC, network, and coverage checks in one report
- **State comparison** — cross-boundary frontend/backend verification
- **Accessibility audits** — WCAG violation assertions
- **Performance budgets** — load time and heap size guards

## Quick Start

```toml
[dev-dependencies]
victauri-test = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

```rust
use victauri_test::{e2e_test, VictauriClient};

e2e_test!(greet_flow, |client| async move {
    client.fill_by_id("name-input", "World").await.unwrap();
    client.click_by_id("greet-btn").await.unwrap();
    client.expect_text("Hello, World!").await.unwrap();
});
```

## Fluent Verification

```rust
let report = client.verify()
    .has_text("Settings saved")
    .ipc_was_called("save_settings")
    .no_console_errors()
    .coverage_above(80.0)
    .run().await?;

report.assert_all_passed();
```

## Visual Regression

```rust
use victauri_test::visual::VisualOptions;

let opts = VisualOptions {
    snapshot_dir: "tests/snapshots".into(),
    threshold_percent: 0.5,
    ..Default::default()
};

let diff = client.screenshot_visual("dashboard", &opts).await?;
assert!(diff.is_match);
```

## IPC Coverage

```rust
use victauri_test::coverage::coverage_report;

let report = coverage_report(&mut client).await?;
assert!(report.meets_threshold(80.0), "{}", report.to_summary());
```

## License

Apache-2.0
