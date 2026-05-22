# Introduction

**Victauri** — Verified Introspection & Control for Tauri Applications.

Victauri gives AI agents X-ray vision and hands inside Tauri apps. Unlike browser automation tools like Playwright (which only see the browser glass), Victauri provides simultaneous access to the webview DOM, the Rust backend, the IPC layer, the database, and native window state — all through a single MCP interface.

## Who Is This For?

- **AI agent developers** who need to test, debug, or drive Tauri applications
- **Tauri app developers** who want automated testing with full-stack visibility
- **QA engineers** looking for deeper inspection than DOM-only tools provide

## Key Value Proposition

One plugin, one line of code, full-stack access:

| Layer | What You Get |
|-------|-------------|
| **WebView** | DOM snapshots, element interaction, JS evaluation, CSS inspection |
| **IPC** | Command registry, invoke commands, intercept and log IPC traffic |
| **Backend** | State reading, memory tracking, process diagnostics |
| **Windows** | Multi-window management, screenshots, positioning |
| **Time-Travel** | Record sessions, checkpoint state, replay events |

All of this is exposed over the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/), the standard that AI agents already speak. Connect Claude Code, VS Code Copilot, or any MCP client — and your agent has complete control of the running Tauri application.

## Design Principles

1. **Same-process** — The MCP server runs inside the Tauri app process, not as a separate sidecar. This gives sub-millisecond tool response times and direct `AppHandle` access.

2. **Zero-cost in release** — Everything is gated behind `#[cfg(debug_assertions)]`. In release builds, the plugin is a complete no-op with zero binary size overhead.

3. **Full-stack** — WebView + IPC + Backend + DB, not just DOM. Cross-boundary verification catches state drift between frontend and backend.

4. **MCP-native** — Speaks the protocol AI agents already understand. No custom SDKs or adapters needed.

5. **Cross-platform** — Works identically on Windows, macOS, and Linux. No CDP dependency.

6. **Plugin, not framework** — One line in `Cargo.toml` to add, one line to remove. Your app architecture stays unchanged.

## Project Structure

Victauri is a Rust workspace with 7 crates:

```
victauri/
├── crates/
│   ├── victauri-browser/    # Chrome extension native host: MCP for any website
│   ├── victauri-cli/        # CLI: init, check, test, record, watch, coverage
│   ├── victauri-core/       # Shared types: events, registry, snapshots
│   ├── victauri-macros/     # Proc macros: #[inspectable]
│   ├── victauri-plugin/     # Tauri plugin: embedded MCP server + JS bridge
│   ├── victauri-test/       # Test client + assertion helpers
│   └── victauri-watchdog/   # Crash-recovery health monitor
├── extensions/
│   └── chrome/              # Chrome/Edge/Brave extension (MV3)
└── examples/
    └── demo-app/            # Reference Tauri app with full test suite
```

## Current Status

All 7 crates are published and production-ready. 1748 tests pass. Tested against 5 real-world open-source Tauri apps (96.9% pass rate across 895 tests) with zero Victauri bugs found. Supports Tauri 2.0+ with rmcp 1.5.0.
