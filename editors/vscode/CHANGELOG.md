# Changelog

All notable changes to the Victauri VS Code extension will be documented in this file.

## 0.8.4 (2026-06-20)

- Version-synced with the Victauri 0.8.4 release (CLI↔plugin version-skew compatibility + in-the-wild
  DX fixes). The stateless MCP transport now backfills a constant `Mcp-Session-Id: stateless` so
  old/strict clients no longer abort the handshake with "no mcp-session-id header"; `victauri check`
  and `doctor` warn on CLI↔plugin version skew; connection-failure diagnostics distinguish auth (401)
  from version skew; and "bridge not responding" errors now name the page-not-loaded case. No extension
  code changes — the extension talks to the embedded server, which carries the fixes.

## 0.8.3 (2026-06-16)

- Version-synced with the Victauri 0.8.3 release (server-side DX/safety fixes, GPT-5.5
  audit-hardened): `screenshot` of a non-visible window now returns a clear error instead of
  silently capturing the wrong window's pixels (both the explicit-label and the omitted-label
  cases), and `query_db` accepts `sql` as an alias for the `query` field. The 0.8.1/0.8.2
  host-crash fixes are also in this line. No extension code changes — the extension talks to the
  embedded server, which carries the fixes.

## 0.8.0 (2026-06-14)

- Version-synced with the Victauri 0.8.0 release. Ships the security hardening from
  the 0.7.10 red-team audit that never reached a published `.vsix` (the marketplace was
  last updated at 0.7.9):
  - **No token leak to auto-discovered services.** A configured `authToken` is now
    treated as an explicit credential for the configured port only — it is no longer
    sent to a different auto-discovered localhost service.
  - **Discovery-tree trust check.** The discovery root is verified as trusted before any
    child entry is read, so a hostile root owner can't swap in a previously-checked dir.
  - Dependency security bumps (pinned `@vscode/vsce`/`ovsx`, refreshed lockfile).
- No new extension features; the Victauri 0.8.0 release itself is a Rust public-API
  cleanup (tool behaviour/output unchanged).

## 0.7.8 (2026-06-06)

- Version-synced with the Victauri 0.7.8 release. Crates-only build-correctness patch:
  `victauri-plugin` now compiles with `default-features = false` and in the release
  profile under `-Dwarnings`. No extension code changes.

## 0.7.7 (2026-06-05)

- Version-synced with the Victauri 0.7.7 release. Crates-only patch: `victauri test`'s
  smoke suite no longer fails on headless CI (the screenshot check tolerates the absence
  of a native window handle). No extension code changes.

## 0.7.6 (2026-06-05)

- Version-synced with the Victauri 0.7.6 release: async-completion awareness
  (`wait_for` gains `expression`/`event` conditions to await fire-and-forget backend
  work without sleeps), the `app_state` tool + `VictauriBuilder::probe` for first-class
  in-process backend-state probes, and actionable connection diagnostics. Plus a
  GPT-5.5 red-team pass (MCP `result.isError` now honored by the SDK, read-only
  `Observe` privacy profile tightened, tool-invocation double-count fixed). **35 tools.**

## 0.7.5 (2026-06-02)

- Version-synced with the Victauri 0.7.5 release (two cross-model red-team passes:
  intent-resolution exact-match ranking fix, `assert_semantic`/`recording.checkpoint`
  UX footgun fixes, bridge-version self-check, ghost-command report split, directory
  traversal guard, and a Chrome-test dependency CVE bump).

## 0.7.3 (2026-06-01)

- Version-synced with the Victauri 0.7.3 release (security-audit hardening: npm RCE
  pin, browser-bridge nonce fix, debugger/CDP permission dropped, command-filter
  enforcement, core DoS bounds, and MCP client 422 stale-session auto-recovery).

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
