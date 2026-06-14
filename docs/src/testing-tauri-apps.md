# Testing Tauri Apps: The Complete Guide

A practical guide to testing Tauri 2.x applications — covering every approach from unit tests to full-stack integration testing.

## The Testing Problem

Tauri apps have three distinct layers that need testing:

1. **Frontend** (HTML/CSS/JS in a webview) — UI rendering, user interactions, client-side state
2. **Backend** (Rust) — business logic, database access, system operations
3. **IPC** (Tauri commands) — the bridge between frontend and backend

Most testing tools only see one layer. Frontend testing tools (Vitest, Playwright) can interact with the DOM but can't verify that the Rust handler ran correctly. Rust testing tools (`cargo test`) can test business logic but can't click a button. The IPC layer — where most Tauri bugs live — falls through the cracks.

This guide covers every approach and when to use each one.

---

## Approach 1: Unit Tests (Rust)

**Best for:** Business logic, data transformations, pure functions.

Standard `cargo test` works perfectly for Rust code that doesn't depend on `AppHandle` or Tauri runtime:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_email() {
        assert!(is_valid_email("alice@example.com"));
        assert!(!is_valid_email("not-an-email"));
    }

    #[test]
    fn calculates_total() {
        let items = vec![Item { price: 10.0, qty: 2 }, Item { price: 5.0, qty: 1 }];
        assert_eq!(calculate_total(&items), 25.0);
    }
}
```

**Limitation:** Can't test anything that touches the Tauri runtime, webview, or IPC layer. If your command handler calls `app.emit()` or reads window state, unit tests won't cover it.

---

## Approach 2: Frontend Tests (Vitest / Jest)

**Best for:** Component rendering, UI logic, client-side state management.

Mock the Tauri IPC layer and test your frontend in isolation:

```typescript
// __mocks__/@tauri-apps/api/core.ts
export const invoke = vi.fn();

// components/Counter.test.ts
import { invoke } from '@tauri-apps/api/core';
import { render, fireEvent } from '@testing-library/svelte';
import Counter from './Counter.svelte';

test('increment calls backend', async () => {
  invoke.mockResolvedValue(1);
  const { getByText } = render(Counter);
  await fireEvent.click(getByText('+'));
  expect(invoke).toHaveBeenCalledWith('increment');
});
```

**Limitation:** You're testing against mocks, not the real backend. The mock says `increment` returns `1`, but the real handler might return an error, use a different type, or have been renamed. Mock drift is the #1 source of false-passing Tauri tests.

---

## Approach 3: WebDriver (Selenium / WebdriverIO)

**Best for:** Teams already invested in WebDriver infrastructure, cross-browser testing.

Tauri supports WebDriver via [tauri-driver](https://crates.io/crates/tauri-driver), which wraps the platform's native WebDriver:

```javascript
// wdio.conf.js
exports.config = {
    capabilities: [{
        'tauri:options': {
            application: '../src-tauri/target/debug/my-app',
        },
    }],
};

// test.js
describe('counter', () => {
    it('increments', async () => {
        await $('[data-testid="increment-btn"]').click();
        const value = await $('[data-testid="counter-value"]').getText();
        expect(value).toBe('1');
    });
});
```

**Limitations:**
- Requires `tauri-driver` binary and platform-specific WebDriver (`msedgedriver` on Windows, `safaridriver` on macOS, `geckodriver` on Linux)
- macOS requires enabling Develop menu and "Allow Remote Automation" in Safari
- Linux requires WebKitGTK WebDriver, which isn't always available
- Can only interact with the DOM — no backend state verification, no IPC inspection
- Slow startup (seconds per test due to WebDriver protocol overhead)

---

## Approach 4: Playwright

**Best for:** Teams familiar with Playwright, visual regression testing.

Playwright doesn't officially support Tauri, but community approaches exist:

```typescript
import { _electron as electron } from 'playwright';

// This only works for Electron apps, not Tauri.
// For Tauri, you'd need to connect to the webview's DevTools port,
// which requires CDP support that varies by platform.
```

**Limitations:**
- No official Tauri support — community workarounds only
- CDP (Chrome DevTools Protocol) availability varies: Windows (WebView2 supports CDP), macOS (WKWebView does not), Linux (WebKitGTK has partial support)
- Cross-platform testing becomes platform-specific
- Same DOM-only limitation as WebDriver

---

## Approach 5: Full-Stack Testing with Victauri

**Best for:** Testing all three layers together — frontend, IPC, and backend — from one test.

[Victauri](https://github.com/runyourempire/victauri) embeds an MCP server inside your Tauri process, giving tests direct access to the DOM, IPC layer, Rust backend, and native windows simultaneously.

### Setup

```bash
cargo install victauri-cli
victauri init
```

Add the plugin to your Tauri app:

```rust
tauri::Builder::default()
    .plugin(victauri_plugin::init())  // no-op in release builds (zero runtime cost)
    .invoke_handler(tauri::generate_handler![greet, increment, list_todos])
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
```

Instrument your commands for full introspection:

```rust
use victauri_macros::inspectable;

#[inspectable(description = "Greet a user by name", category = "ui")]
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}
```

### Writing Tests

```rust
use victauri_test::{e2e_test, locator::Locator};
use serde_json::json;

// Basic interaction test
e2e_test!(greet_flow, |client| async move {
    // Fill the input
    client.fill_by_id("name-input", "Alice").await.unwrap();

    // Click the button
    client.click_by_id("greet-btn").await.unwrap();

    // Verify the UI updated
    client.expect_text("Hello, Alice!").await.unwrap();
});
```

### The Locator API

Composable element queries inspired by Playwright:

```rust
e2e_test!(locator_example, |client| async move {
    Locator::test_id("name-input")
        .fill(&mut client, "Bob")
        .await
        .unwrap();

    Locator::text("Greet")
        .click(&mut client)
        .await
        .unwrap();

    Locator::test_id("greet-result")
        .expect(&mut client)
        .to_contain_text("Hello, Bob!")
        .await
        .unwrap();
});
```

### Cross-Boundary Verification

Test that the DOM and backend agree — the pattern that catches state drift:

```rust
e2e_test!(counter_state_sync, |client| async move {
    client.invoke_command("reset_counter", None).await.unwrap();

    // Interact via UI
    client.click_by_id("increment-btn").await.unwrap();
    client.click_by_id("increment-btn").await.unwrap();

    // Verify both layers agree
    let report = client.verify()
        .state_matches(
            "parseInt(document.getElementById('counter-value').textContent)",
            json!({"counter": 2}),
        )
        .ipc_was_called("increment")
        .no_console_errors()
        .run()
        .await
        .unwrap();

    report.assert_all_passed();
});
```

### IPC Testing

Verify that commands exist, were called, and return the right data:

```rust
e2e_test!(ipc_verification, |client| async move {
    // Check IPC layer health
    let report = client.check_ipc_integrity().await.unwrap();
    assert!(report["healthy"].as_bool().unwrap());

    // Invoke a command directly and check the result
    let todo: serde_json::Value = client
        .invoke_command("add_todo", Some(json!({"title": "Write tests"})))
        .await
        .unwrap();
    assert!(todo["id"].is_number());

    // Find ghost commands — frontend calls with no backend handler
    let ghosts = client.detect_ghost_commands().await.unwrap();
    assert!(ghosts["ghosts"].as_array().unwrap().is_empty(),
        "found ghost commands: {:?}", ghosts);

    // Check command registry
    let registry = client.get_registry().await.unwrap();
    let names: Vec<&str> = registry.as_array().unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(names.contains(&"add_todo"));
});
```

### Accessibility Auditing

WCAG checks built in — no external tools needed:

```rust
e2e_test!(accessibility_check, |client| async move {
    let audit = client.audit_accessibility().await.unwrap();
    let violations = audit["summary"]["violations"].as_u64().unwrap_or(0);

    assert!(violations == 0,
        "a11y violations found: {}",
        serde_json::to_string_pretty(&audit["violations"]).unwrap_or_default()
    );
});
```

### Performance Budgets

Enforce performance limits in CI:

```rust
e2e_test!(performance_budget, |client| async move {
    let perf = client.get_performance_metrics().await.unwrap();

    // DOM interactive under 3 seconds
    if let Some(ms) = perf["navigation"]["domInteractive"].as_f64() {
        assert!(ms < 3000.0, "DOM interactive: {ms}ms");
    }

    // JS heap under 100MB
    if let Some(mb) = perf["jsHeap"]["usedMB"].as_f64() {
        assert!(mb < 100.0, "JS heap: {mb}MB");
    }

    // Under 500 DOM elements
    if let Some(count) = perf["dom"]["elementCount"].as_u64() {
        assert!(count < 500, "DOM elements: {count}");
    }
});
```

### Multi-Window Testing

Test apps with multiple windows:

```rust
e2e_test!(multi_window, |client| async move {
    let windows = client.list_windows().await.unwrap();
    let labels: Vec<&str> = windows.as_array().unwrap()
        .iter()
        .filter_map(|w| w.as_str())
        .collect();
    assert!(labels.contains(&"main"));

    // Check state of specific window
    let state = client.get_window_state(Some("main")).await.unwrap();
    assert!(state["visible"].as_bool().unwrap());
    assert!(state["width"].as_f64().unwrap() > 0.0);
});
```

### Time-Travel Recording

Record interactions and replay them:

```rust
e2e_test!(recording, |client| async move {
    client.start_recording(None).await.unwrap();

    // Do some actions
    client.invoke_command("increment", None).await.unwrap();
    client.invoke_command("increment", None).await.unwrap();

    let session = client.stop_recording().await.unwrap();
    let events = session["events"].as_array().unwrap();
    assert!(!events.is_empty());
});
```

### The Smoke Suite

Built-in checks that run in seconds:

```bash
# CLI — 11 checks, pass/fail, JUnit XML
victauri test --max-load-ms 5000 --max-heap-mb 256 --junit results.xml

# From code
e2e_test!(smoke, |client| async move {
    let report = client.smoke_test().await.unwrap();
    assert!(report.all_passed(),
        "{}/{} passed", report.passed_count(), report.total_count());
});
```

### IPC Coverage

Know which commands your tests exercise:

```bash
victauri coverage --threshold 80
```

Output:
```
IPC Command Coverage Report
────────────────────────────
  greet              ✓ covered
  increment          ✓ covered
  add_todo           ✓ covered
  delete_todo        ✗ NOT covered
  update_settings    ✗ NOT covered

Coverage: 3/5 commands (60.0%)
✗ Below threshold of 80%
```

---

## Comparison

| | Unit tests | Frontend mocks | WebDriver | Playwright | **Victauri** |
|---|---|---|---|---|---|
| **DOM interaction** | - | Yes | Yes | Yes | Yes |
| **Backend verification** | Yes | - | - | - | Yes |
| **IPC inspection** | - | Mocked | - | - | Real |
| **Cross-boundary** | - | - | - | - | Yes |
| **Ghost detection** | - | - | - | - | Yes |
| **A11y auditing** | - | Via lib | - | Yes | Yes |
| **Perf profiling** | - | - | - | Yes | Yes |
| **Screenshots** | - | - | Yes | Yes | Yes |
| **Setup complexity** | None | Low | High | Medium | Low |
| **Cross-platform** | Yes | Yes | Varies | Varies | Yes |
| **Release overhead** | None | None | None | None | None |
| **AI agent support** | - | - | - | - | MCP + REST |

---

## Recommended Strategy

Use all the approaches where they shine:

1. **Unit tests** for pure business logic (no Tauri runtime needed)
2. **Frontend tests** for component-level rendering (mock only when intentional)
3. **Victauri** for integration tests that verify frontend + IPC + backend work together
4. **Victauri CLI** in CI as a smoke gate before merge

```
Unit tests ─────────────────── cargo test (fast, Rust-only)
                                    │
Frontend tests ─────────────── vitest / jest (component rendering)
                                    │
Integration tests ──────────── victauri e2e_test! (full-stack)
                                    │
CI smoke gate ──────────────── victauri test (11 checks, seconds)
                                    │
Coverage gate ──────────────── victauri coverage --threshold 80
```

---

## CI Integration

### GitHub Action

```yaml
- name: Start app
  run: xvfb-run --auto-servernum cargo run -p my-app &

- uses: runyourempire/victauri/.github/actions/victauri-test@v0.8.1
  with:
    max-load-ms: 5000
    coverage: true
    coverage-threshold: 80
    junit-path: results.xml
```

### Manual

```yaml
- name: Start app
  run: xvfb-run --auto-servernum cargo run -p my-app &

- name: Wait for server
  run: |
    for i in $(seq 1 30); do
      curl -sf http://127.0.0.1:7373/health && break
      sleep 1
    done

- name: Test
  run: victauri test --junit results.xml

- name: Coverage
  run: victauri coverage --threshold 80
```

### Platform Notes

| Platform | Display server | Screenshot method |
|---|---|---|
| Linux | `xvfb-run --auto-servernum` | X11 `GetImage` (pure Wayland fails safely) |
| macOS | None needed | `CGWindowListCreateImage` |
| Windows | None needed | `PrintWindow` + `GetDIBits` |

---

## REST API

Every Victauri tool is also accessible via plain HTTP — useful for scripts, CI pipelines, or any language:

```bash
# List tools
curl http://127.0.0.1:7373/api/tools

# Evaluate JS
curl -X POST http://127.0.0.1:7373/api/tools/eval_js \
  -H "Content-Type: application/json" \
  -d '{"code": "document.title"}'

# Get memory stats
curl -X POST http://127.0.0.1:7373/api/tools/get_memory_stats

# Take screenshot
curl -X POST http://127.0.0.1:7373/api/tools/screenshot -d '{}'
```

---

## Further Reading

- [Victauri README](https://github.com/runyourempire/victauri) — full tool reference, architecture, quick start
- [Demo app tests](https://github.com/runyourempire/victauri/tree/main/examples/demo-app/tests) — 20 integration tests demonstrating every pattern
- [Agent session example](https://github.com/runyourempire/victauri/blob/main/examples/agent-session.md) — real AI agent session transcript
- [Tauri testing docs](https://v2.tauri.app/develop/tests/) — official Tauri testing guidance
- [MCP protocol](https://modelcontextprotocol.io) — the protocol Victauri speaks
