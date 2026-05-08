# victauri-cli

CLI toolkit for [Victauri](https://github.com/runyourempire/victauri) — scaffold tests, check running apps, record sessions, measure coverage.

## Install

```bash
cargo install victauri-cli
```

## Commands

### `victauri init [path]`

Scaffold a test directory with starter smoke tests. Detects your Tauri project, adds dependencies, and generates initial test files.

```bash
victauri init
victauri init ./my-tauri-app
```

### `victauri check`

Connect to a running Tauri app and report health — IPC integrity, ghost commands, memory usage.

```bash
victauri check
victauri check --junit report.xml   # JUnit XML output for CI
```

### `victauri record`

Record user interactions from a running app and generate a Rust test file.

```bash
victauri record                             # Interactive — Ctrl+C to stop
victauri record --output tests/login.rs     # Write to specific file
victauri record --test-name login_flow      # Custom test function name
```

Generated tests use idiomatic Victauri API — `click_by_id` for `#id` selectors, `click_by_text` for text selectors, with timing comments for pauses between actions.

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

## License

Apache-2.0
