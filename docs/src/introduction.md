# Introduction

**Victauri** — Verified Introspection & Control for Tauri Applications.

Victauri is full-stack testing for Tauri apps. Click a button in the frontend, verify the Rust command ran, confirm the database row was written — from a single test, on macOS, Windows, and Linux, in CI. Unlike browser automation tools like Playwright (which only see the browser glass), Victauri has simultaneous access to the webview DOM, the IPC layer, the Rust backend, the database, and native window state.

It works by embedding a lightweight server inside your Tauri app's own process — debug builds only; it compiles away to nothing in release. Your test suite, `curl`, or CI talks to it over a plain REST/HTTP API. No WebDriver, no Selenium grid, no browser dependency.

And because that same server also speaks the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/), any AI agent — Claude Code, Cursor, Windsurf — gets the exact same full-stack access for interactive debugging. **Testing is the job; the agent integration is the bonus.**

## Who Is This For?

- **Tauri app developers** who want real full-stack tests (frontend → IPC → Rust → database) instead of frontend mocks that lie about the backend
- **QA and CI engineers** who need cross-platform end-to-end tests without standing up a WebDriver/Selenium grid or paying for macOS runners
- **AI agent developers** who need to drive, debug, or inspect a running Tauri application over MCP

## Key Value Proposition

One plugin, one line of code, full-stack access:

| Layer | What You Get |
|-------|-------------|
| **WebView** | DOM snapshots, element interaction, JS evaluation, CSS inspection |
| **IPC** | Command registry, invoke commands, intercept and log IPC traffic |
| **Backend** | State reading, memory tracking, process diagnostics |
| **Windows** | Multi-window management, screenshots, positioning |
| **Time-Travel** | Record sessions, checkpoint state, replay events |

All of this is exposed two ways from the same server: a plain **REST/HTTP API** (`POST /api/tools/{name}`) that your test suite, shell scripts, and CI call directly — no handshake, no session — and the **[Model Context Protocol (MCP)](https://modelcontextprotocol.io/)** for AI agents. Write deterministic tests against REST; connect Claude Code, Cursor, or any MCP client when you want an agent to drive the app interactively.

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
│   ├── chrome/              # Chrome/Edge/Brave extension (MV3)
│   ├── firefox/             # Firefox extension (MV3)
│   └── npm/                 # victauri-browser npm package
├── editors/
│   └── vscode/              # VS Code extension
└── examples/
    └── demo-app/            # Reference Tauri app with full test suite
```

## Current Status

All 7 crates are published to crates.io. In our own compatibility testing against 5 real-world open-source Tauri apps (Kanri, En Croissant, Surrealist, Duckling, Lettura), 867 of 895 tests passed (96.9%) with zero Victauri bugs and zero changes required to the apps — the remaining failures were test-script issues or correct actionability enforcement. Supports Tauri 2.0+ with rmcp 1.5.0.

Victauri is open source (Apache-2.0) and built by [4DA Systems](https://4da.ai), which uses it to test its own Tauri app. **Adopters and contributors are very welcome** — see [Contributing](https://github.com/runyourempire/victauri/blob/main/CONTRIBUTING.md), and if you ship a Tauri app we'd love to hear how it goes.
