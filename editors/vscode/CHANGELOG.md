# Changelog

All notable changes to the Victauri VS Code extension will be documented in this file.

## 0.7.2 (2026-05-31)

- Version-synced with the Victauri workspace 0.7.2 release (animation-debugging suite, per-window introspectability diagnostic, loud-fail on blank window capture).
- Connects to the embedded Victauri MCP/REST server exposing all 34 tools.

## 0.2.0 (2026-05-14)

- New logo — minimalist glowing V with electric blue glow on dark background
- Updated gallery banner color to match new branding
- Cleaned up VSIX packaging (117 KB, no unused assets)

## 0.1.1 (2026-05-14)

- Improved marketplace listing with badges, compatibility table, and better documentation
- Added keywords for discoverability
- Added Quick Start guide

## 0.1.0 (2026-05-14)

Initial release.

### Features

- **Activity Bar** — three tree views: App State, DOM Explorer, IPC Log
- **Live Polling** — windows, memory, DOM, IPC traffic, performance metrics, diagnostics
- **Screenshot Panel** — inline webview with refresh and save-to-disk
- **CodeLens** — "Generate Victauri test" on every `#[tauri::command]` in Rust files
- **DOM Interactions** — right-click to click, highlight, inspect styles, copy ref IDs, generate tests
- **Accessibility Audit** — comprehensive WCAG checks with violation reporting
- **Performance Metrics** — navigation timing, JS heap, DOM stats, long tasks, resources
- **Diagnostics** — CSP, bridge method, shadow DOM, service worker compatibility checks
- **Evaluate JS** — run JavaScript in the Tauri webview via input box
- **Status Bar** — connection state indicator with click-to-connect
- **Auto-Connect** — detects `tauri.conf.json` in workspace and connects on activation
- **Port Discovery** — reads `victauri.port` from temp directory for non-default ports
- **Bearer Token Auth** — supports authenticated connections via settings
