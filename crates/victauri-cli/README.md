# victauri-cli

CLI toolkit for [Victauri](https://github.com/4DA-Systems/victauri) — scaffold tests, diagnose setup, check running apps, record sessions, measure coverage.

## Install

```bash
cargo install victauri-cli
```

## Commands

### `victauri init [path]`

Zero-config setup for any Tauri project. Detects your project, adds dependencies, auto-patches your Tauri builder to wire the plugin, creates `.mcp.json` and capabilities, and generates starter test files.

```bash
victauri init
victauri init ./my-tauri-app
```

What it does:
1. Adds `victauri-plugin` and `victauri-core` to `Cargo.toml`
2. Patches `src/main.rs` or `src/lib.rs` to insert `.plugin(victauri_plugin::init())`
3. Creates `.mcp.json` for Claude Code connection
4. Creates `capabilities/victauri.json` for Tauri permissions
5. Generates `tests/smoke.rs` and `tests/integration.rs` templates

### `victauri doctor`

Comprehensive diagnostic — checks every step from project structure to live tool operation. Run this when something isn't working.

```bash
victauri doctor
```

Checks: project structure, Tauri dependency, plugin dependency, test dependency, plugin wiring in source, `.mcp.json`, capabilities, test files, server connectivity, plugin info, JS bridge, DOM snapshot, IPC integrity.

### `victauri check`

Connect to a running Tauri app and report health — IPC integrity, ghost commands, memory usage.

```bash
victauri check
victauri check --junit report.xml   # JUnit XML output for CI
```

### `victauri test`

Run the built-in smoke test suite (11 checks) against a running app. Exits 0/1 for CI.

```bash
victauri test
victauri test --max-load-ms 3000    # Custom load time budget
victauri test --max-heap-mb 200     # Custom heap budget
victauri test --junit results.xml   # JUnit XML output
```

### `victauri record`

Record user interactions from a running app and generate a Rust test file.

```bash
victauri record                             # Direct client API style
victauri record --locator                   # Locator API style output
victauri record --assert-ipc               # Add IPC assertion calls
victauri record --output tests/login.rs     # Write to specific file
victauri record --test-name login_flow      # Custom test function name
```

With `--locator`, generated code uses `Locator::test_id("btn").click(&mut client)` instead of `client.click_by_id("btn")`. With `--assert-ipc`, each IPC command seen during recording gets an `assert_ipc_called()` assertion at the end.

### `victauri coverage`

Report IPC command coverage — which registered commands your tests exercise.

```bash
victauri coverage                    # Print coverage report
victauri coverage --threshold 80     # Exit code 1 if below 80%
victauri coverage --junit cov.xml    # JUnit XML output
```

### `victauri watch`

Watch test files and re-run on changes — with 300ms debounce.

```bash
victauri watch                           # Watch default test directory
victauri watch --dir tests/integration   # Watch specific directory
victauri watch --filter greet            # Only run matching tests
```

## Documentation

Full API docs: [docs.rs/victauri-cli](https://docs.rs/victauri-cli)

## License

Apache-2.0 -- see [LICENSE](../../LICENSE)

Part of [Victauri](https://github.com/4DA-Systems/victauri). Built by [4DA Systems](https://4da.ai).
