# Migration Guide

## v0.1.x → v0.2.0

### Breaking Changes

#### `file:` URL navigation blocked by default

The `navigate` tool's `go_to` action now rejects `file://` URLs by default to prevent local filesystem access. If your tests navigate to local HTML files, opt in explicitly:

```rust
VictauriBuilder::new()
    .allow_file_navigation()
    .build()
```

#### `DomElement.attributes` and `DomSnapshot.ref_map` use `BTreeMap`

These fields changed from `HashMap<String, String>` to `BTreeMap<String, String>` for deterministic serialization order. Update your imports:

```rust
// Before
use std::collections::HashMap;
// After
use std::collections::BTreeMap;
```

The API is identical — `BTreeMap` implements the same traits. If you were constructing `DomElement` or `DomSnapshot` in test code, change `HashMap::new()` to `BTreeMap::new()`.

#### Strict privacy mode blocks more tools

`strict_privacy_mode()` now also blocks:
- `invoke_command` — was completely ungated in v0.1.x
- `window.manage`, `window.resize`, `window.move_to`, `window.set_title` — window mutation actions

If your strict-mode tests used these tools, either switch to default mode or selectively re-enable them via `disable_tools()`.

#### `TestError::Connection` is now a struct variant

`TestError::Connection(String)` changed to `TestError::Connection { host, port, reason }` for richer diagnostics. Update pattern matches:

```rust
// Before
Err(TestError::Connection(msg)) => eprintln!("{msg}"),

// After
Err(TestError::Connection { host, port, reason }) => {
    eprintln!("failed to reach {host}:{port}: {reason}");
}
```

### New Features

- **`VictauriBuilder::allow_file_navigation()`** — opt in to `file://` URL navigation
- **Mutex poisoning recovery** — all lock acquisitions now use `acquire_lock`/`acquire_read`/`acquire_write` helpers with tracing diagnostics instead of panicking
- **Origin guard hardening** — URL-parsing based validation replaces vulnerable `starts_with` pattern
- **Centralized output redaction** — redaction applies uniformly at the `call_tool` boundary, not per-tool
- **TestApp stderr capture** — connection timeout errors now include the last 10 lines of the app's stderr
- **CLI empty registry warning** — `victauri coverage` exits with code 1 when no commands are registered (use `--allow-empty-registry` to override)
- **Release workflow hardening** — dry-run gate before real publishes, no `continue-on-error`

### Deprecations

- **`ipc_checkpoint()`** → renamed to **`create_ipc_checkpoint()`** (verb-first naming). The old name still works with a deprecation warning and forwards to the new method.
