# Contributing to Victauri

## Getting Started

```bash
git clone https://github.com/runyourempire/victauri.git
cd victauri
cargo build --workspace
cargo test --workspace
```

Requires Rust 1.88+ (edition 2024).

## Running Tests

```bash
cargo test --workspace          # All tests (756)
cargo test -p victauri-core     # Core crate only
cargo test -p victauri-plugin   # Plugin crate only
cargo bench -p victauri-core    # Criterion benchmarks (16)
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

All checks (test, clippy, fmt) must pass. CI runs them on Linux, Windows, and macOS.

## Code Style

- `cargo fmt --all` before committing
- `cargo clippy --workspace --all-targets` must pass with zero warnings (20 clippy lints enforced at deny level — see `[workspace.lints.clippy]` in `Cargo.toml`)
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` must pass
- All public `Result`-returning functions must have `# Errors` doc sections
- All functions that may panic must have `# Panics` doc sections
- All value-returning public functions must have `#[must_use]`
- Prefer `let...else` over match for single-pattern extraction with early return
- Prefer `map_or` over `map().unwrap_or()`
- Use method references (`PoisonError::into_inner`) over closures (`|e| e.into_inner()`)
- No `unwrap()` on mutexes or RwLocks — use `unwrap_or_else(PoisonError::into_inner)` for poisoning recovery
- No `unreachable!()` in match arms that could be reached by malformed input
- `unsafe_code = "deny"` is enforced workspace-wide — use targeted `#[allow(unsafe_code)]` with `// SAFETY:` comments for FFI

## Structure

| Crate | Purpose |
|---|---|
| `victauri-core` | Types, events, verification, registry — no Tauri dependency |
| `victauri-macros` | `#[inspectable]` proc macro |
| `victauri-plugin` | Tauri plugin: MCP server, JS bridge, auth, privacy |
| `victauri-test` | Typed MCP HTTP client + assertion helpers for CI testing |
| `victauri-watchdog` | Health-check sidecar |

## Adding MCP Tools

1. Add the `#[tool]` handler in `crates/victauri-plugin/src/mcp/mod.rs`
2. Add parameter struct in the appropriate `*_params.rs` module
3. Add tests in the appropriate test module
4. Update the tool count in README and CHANGELOG

## Demo App

The demo app at `examples/demo-app/` exercises all command patterns. After changing the plugin API, verify the demo still builds:

```bash
cargo build -p demo-app
```

## Publishing to crates.io

Maintainers only. Push a `v*` tag and the release workflow handles publication in dependency order:

```
victauri-core -> victauri-macros -> victauri-plugin -> victauri-test -> victauri-watchdog
```

## License

By contributing, you agree that your contributions will be licensed under Apache-2.0.
