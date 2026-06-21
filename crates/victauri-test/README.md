# victauri-test

Playwright-style testing for Tauri apps via [Victauri](https://github.com/4DA-Systems/victauri).

## What It Does

Typed HTTP client for the Victauri MCP server with high-level convenience methods:

- **Playwright-style API** — `click_by_text`, `fill_by_id`, `expect_text`, `select_by_id`
- **Locator API** — composable element queries: `Locator::role("button").and_text("Save")`
- **IPC verification** — call logs, checkpoints, ghost command detection, coverage tracking
- **Visual regression** — pixel-level screenshot comparison with baseline snapshots
- **Fluent assertions** — chain DOM, IPC, network, and coverage checks in one report
- **State comparison** — cross-boundary frontend/backend verification
- **Accessibility audits** — WCAG violation assertions
- **Performance budgets** — load time and heap size guards
- **Smoke test suite** — 11 built-in checks with pass/fail reporting

## Quick Start

```toml
[dev-dependencies]
victauri-test = "0.5"
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

## Locator API

Composable element queries with factory methods, refinement, actions, and auto-waiting expectations:

```rust
use victauri_test::locator::Locator;

// Factory → refinement → action
Locator::role("button").and_text("Save").click(&mut client).await?;
Locator::test_id("email-input").fill(&mut client, "user@example.com").await?;
Locator::placeholder("Search...").fill(&mut client, "query").await?;

// Expectations with auto-wait
Locator::test_id("toast")
    .expect(&mut client)
    .to_be_visible()
    .await?;

Locator::css("#counter")
    .expect(&mut client)
    .to_have_text("42")
    .await?;
```

### Factory methods

| Method | Finds by |
|---|---|
| `Locator::role("button")` | ARIA role |
| `Locator::text("Submit")` | Visible text (substring) |
| `Locator::text_exact("OK")` | Visible text (exact) |
| `Locator::test_id("save-btn")` | `data-testid` attribute |
| `Locator::css("#my-id")` | CSS selector |
| `Locator::label("Email")` | Associated `<label>` text |
| `Locator::placeholder("Search")` | Placeholder text |
| `Locator::alt_text("Logo")` | Image alt text |
| `Locator::title("Close")` | Title attribute |

### Refinement

Chain `.and_text()`, `.and_role()`, `.and_tag()`, `.nth()`, `.first()`, `.last()` to narrow results.

### Expectations

`locator.expect(&mut client)` returns an expectation builder with:
`to_be_visible()`, `to_be_hidden()`, `to_be_enabled()`, `to_be_disabled()`,
`to_have_text("...")`, `to_contain_text("...")`, `to_have_value("...")`,
`to_have_role("...")`, `to_have_attribute("name", "value")`,
`to_have_count(n)`, `to_have_bounds_near(...)`, `not()` (negate any).

## Fluent Verification

```rust
let report = client.verify()
    .has_text("Settings saved")
    .has_no_text("Error")
    .ipc_was_called("save_settings")
    .ipc_was_called_with("save_settings", json!({"theme": "dark"}))
    .ipc_was_not_called("delete_account")
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

## Smoke Test

```rust
let report = client.smoke_test().await?;
report.assert_all_passed();
```

Runs 11 checks: health, DOM snapshot, eval, IPC integrity, registry, window state, screenshot, memory, console errors, performance, ghost commands.

## Documentation

Full API docs: [docs.rs/victauri-test](https://docs.rs/victauri-test)

## License

Apache-2.0 -- see [LICENSE](../../LICENSE)

Part of [Victauri](https://github.com/4DA-Systems/victauri). Built by [4DA Systems](https://4da.ai).
