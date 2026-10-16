# How Victauri Compares

Several tools let you (or an AI agent) drive and inspect a Tauri app. They make different
trade-offs, and the right one depends on what you need. This page tries to be fair about it:
where another tool is the better choice, it says so.

Facts about other projects come from their own documentation and source, checked on
**2026-10-16**, with links. If something here is out of date or wrong, please open an issue and
we will correct it.

## The short version

- **Victauri** sits inside the app process on both sides of the IPC boundary. It is strongest at
  *reading the backend*: the app's SQLite database (read-only), app-defined state probes, IPC
  calls with request and response bodies, IPC contract checks, and recording with checkpoints,
  all through one MCP server that is authenticated and localhost-only by default. It is
  desktop-only and young.
- **hypothesi/mcp-server-tauri** is the closest alternative for AI agents. It has much better
  distribution (an installer for about a dozen MCP clients, agent skills, a Claude Code plugin,
  a Gemini extension), and it supports **mobile** (Android and iOS emulators and simulators).
- **WebdriverIO's Tauri service** is the choice if you want **W3C WebDriver** and a standard
  JavaScript test runner (Mocha, Jasmine, Cucumber) with its reporters.
- **tauri-pilot** is a lean CLI plus MCP server with owner-only local sockets, TOML test
  scenarios, and Android and iOS Simulator support.
- **Playwright over CDP** gives you full Playwright depth (network routing, tracing, the test
  runner), but only on **Windows** (WebView2), and only at the webview layer.
- **CrabNebula DevTools** is a polished, human-facing inspector for logs, tracing spans, IPC
  calls and config. It is not an automation or agent tool.

## Capability matrix

"Not documented" means we found nothing in the project's docs or source either way. It does
**not** mean the feature is confirmed absent.

| | Victauri | hypothesi/mcp-server-tauri | WebdriverIO Tauri service | tauri-pilot | Playwright / CDP (WebView2) | CrabNebula DevTools |
|---|---|---|---|---|---|---|
| **Primary audience** | AI agents and Rust tests | AI agents | Test suites (JS) | CLI and AI agents | Test suites (JS/TS/Python/…) | Humans (DevTools UI) |
| **Interface** | MCP (Streamable HTTP, plus a stdio bridge) and REST, embedded in the app | MCP (stdio Node server) ↔ WebSocket ↔ Tauri plugin | W3C WebDriver | CLI; `tauri-pilot mcp` stdio server; JSON-RPC to the plugin | Chrome DevTools Protocol | gRPC / gRPC-web to a web UI |
| **Database access** | Yes: `query_db` and `db_health` on the app's SQLite, read-only (enforced by SQLite's authorizer) | Not documented | Not documented | Not documented | No (webview only) | Not documented |
| **IPC capture with bodies** | Yes: request args and response body, captured continuously | Yes: opt-in monitor recording command, args, result, error and duration | Not documented (it can mock the invoke API) | No: the network log records sizes, not bodies | Partial: Tauri IPC appears as network requests on Windows | Yes: a "Calls" view with arguments and return values |
| **App-defined backend state** | Yes: `app_state` probes registered by the app | Fixed metadata (app, Tauri version, windows) | No | No | No | No (shows config, logs and spans) |
| **Recording / replay** | Yes: event recording with checkpoints, export/import, replay of argument-free IPC commands | Not documented | No (test-runner based) | Yes: record and replay UI interactions, export to a shell script | Playwright tracing (Windows only) | Not documented |
| **Network mocking** | `route`: block, fulfill or delay fetch/XHR. Tauri IPC is observe-only | Not documented | Mocks Tauri's invoke API | Not documented | Yes: `page.route` | No |
| **IPC contract / schema-drift checks** | Yes (`introspect contract_*`, `command_catalog`) | Not documented | No | No | No | No |
| **Default bind address** | `127.0.0.1` only (port 7373, falls back through 7383) | `0.0.0.0`, documented as being for remote-device support; `Config::localhost_only()` is available | Embedded driver: `127.0.0.1:4445` | Unix socket / Windows named pipe (no TCP) | Whatever `--remote-debugging-port` is set to | `127.0.0.1:3033` on desktop, all interfaces on mobile |
| **Authentication** | On by default: a per-run Bearer token in an owner-only, per-user discovery directory, plus DNS-rebinding and Origin guards | None documented | None documented | Owner-only socket or pipe permissions; no separate handshake | Not documented by Playwright | None documented |
| **Release builds** | Inert by default: the plugin gates itself on `debug_assertions` | Users are told to wrap it in `#[cfg(debug_assertions)]` themselves | The docs warn never to ship it in production | Debug-only | Do not ship the debug port | The docs advise disabling it in production |
| **Desktop platforms** | Windows, macOS, Linux | Windows, macOS, Linux | Windows, macOS, Linux | Windows, macOS, Linux | **Windows only** | Windows, macOS, Linux |
| **Mobile** | **No** | **Yes**: Android and iOS (emulators and simulators) | Not documented | **Yes**: Android (via ADB) and the iOS Simulator | No | Android emulator documented |
| **Trusted (OS-level) input** | Windows only (`SendInput`); synthetic events elsewhere | Not documented | Via WebDriver | Not documented | Via CDP (Windows) | No |
| **Standards / test runner** | Rust: `victauri-test` with `cargo test`, JUnit XML from the CLI, a GitHub Action. **Not** WebDriver | MCP only | **W3C WebDriver** + WDIO runner and reporters | TOML scenarios with `--junit` | Playwright Test | None |
| **Agent onboarding** | `victauri setup <client>` for 8 clients: Claude Code, Cursor, VS Code/Copilot, Windsurf/Devin, Codex, Gemini CLI, Zed, Roo; plus CLAUDE.md, AGENTS.md and a Claude Code skill | Installer via the third-party `aix` tool for 11 clients, plus agent skills, a Claude Code plugin marketplace entry and a Gemini extension | n/a | Agent skill via `npx skills add` | n/a | n/a |
| **License** | Apache-2.0 | MIT | MIT | MIT | Apache-2.0 | Apache-2.0/MIT for `wire` and `devtools`; PolyForm Noncommercial for the rest |
| **Maturity (2026-10-16)** | v0.9.0; repo created 2026-04; 4 GitHub stars | v0.13.1; created 2025-11; ~320 stars; frequent releases | `@wdio/tauri-service` 1.5.0; official Tauri docs point to it | v0.8.1; created 2026-04; ~86 stars | Mature (Playwright) | ~430 stars; tags up to v0.3.5 |

## Where the others win

Be clear-eyed about these before you choose:

- **Mobile.** Victauri does not support Android or iOS. If you ship a Tauri mobile app, look at
  hypothesi/mcp-server-tauri or tauri-pilot.
- **Distribution and agent onboarding.** hypothesi's project reaches more clients, through more
  channels (an installer, skills, a Claude Code plugin, a Gemini extension), and has a much
  larger user base. `victauri setup` closes part of this gap, but Victauri has no plugin
  marketplace entries.
- **Standards.** If your team standardises on W3C WebDriver, or wants a JavaScript test runner
  with its ecosystem of reporters, the WebdriverIO Tauri service is built for exactly that.
  Victauri's test client is Rust-first.
- **CDP depth on Windows.** On WebView2, Playwright over CDP offers things Victauri does not:
  JS/CSS coverage, CPU throttling, cross-origin frames, and interception of top-level
  navigation and WebSockets. CDP can also attach to a window whose Tauri capability does not
  grant `victauri:default`, because it sits below Tauri's permission layer.
- **Native input outside Windows.** Victauri's trusted (OS-level) input is Windows-only. On
  macOS and Linux it falls back to synthetic DOM events (`isTrusted: false`).
- **Faulting the real frontend IPC.** Victauri's `fault` tool and `route` rules do not
  intercept the app's own `invoke()` calls; they apply to commands Victauri drives. The
  WebdriverIO service's invoke mocking targets the frontend's own calls.
- **A human-facing UI.** For browsing logs, tracing spans and IPC calls by eye, CrabNebula
  DevTools has a far better interface. Victauri has no GUI beyond a VS Code extension.

## Where Victauri is different

- **It reads the backend directly**, not just the webview: the app's SQLite database (read-only,
  bounded and root-contained), app-defined `app_state` probes, command timings, the event bus,
  and IPC contract baselines. None of the other tools document database access.
- **One process, both sides of the boundary.** The MCP server runs inside the Tauri process, so
  backend tools need no webview round trip, and `verify_state` can compare frontend and backend
  state in one call.
- **Locked down by default.** It binds only to localhost, requires authentication by default, has
  host and origin guards, and is inert in release builds without any action on your part. See
  [Security](./security.md).
- **The same tools on all three desktop engines** (WebView2, WKWebView, WebKitGTK), with no CDP
  dependency.

## Sources

- hypothesi/mcp-server-tauri:
  [README](https://github.com/hypothesi/mcp-server-tauri/blob/main/README.md),
  [plugin `config.rs`](https://github.com/hypothesi/mcp-server-tauri/blob/main/packages/tauri-plugin-mcp-bridge/src/config.rs),
  [plugin README](https://github.com/hypothesi/mcp-server-tauri/blob/main/packages/tauri-plugin-mcp-bridge/README.md),
  [IPC monitor](https://github.com/hypothesi/mcp-server-tauri/blob/main/packages/tauri-plugin-mcp-bridge/src/monitor.rs),
  [tool registry](https://github.com/hypothesi/mcp-server-tauri/blob/main/packages/mcp-server/src/tools-registry.ts),
  [agent skills guide](https://github.com/hypothesi/mcp-server-tauri/blob/main/docs/guides/agent-skills.md),
  [Claude Code marketplace entry](https://github.com/hypothesi/mcp-server-tauri/blob/main/.claude-plugin/marketplace.json),
  [Gemini extension](https://github.com/hypothesi/mcp-server-tauri/blob/main/gemini-extension.json),
  [mobile development](https://github.com/hypothesi/mcp-server-tauri/blob/main/docs/api/mobile-development.md)
- Tauri WebDriver: [official guide](https://v2.tauri.app/develop/tests/webdriver/),
  [tauri-driver README](https://github.com/tauri-apps/tauri/blob/dev/crates/tauri-driver/README.md)
- WebdriverIO: [Tauri service docs](https://webdriver.io/docs/desktop-testing/tauri),
  [service README](https://github.com/webdriverio/desktop-mobile/blob/main/packages/tauri-service/README.md),
  [`tauri-plugin-wdio-webdriver`](https://docs.rs/crate/tauri-plugin-wdio-webdriver/1.3.0/source/README.md)
- tauri-pilot: [README](https://github.com/mpiton/tauri-pilot/blob/main/README.md),
  [SECURITY.md](https://github.com/mpiton/tauri-pilot/blob/main/SECURITY.md),
  [bridge.js](https://github.com/mpiton/tauri-pilot/blob/main/crates/tauri-plugin-pilot/js/bridge.js)
- Playwright: [WebView2 guide](https://playwright.dev/docs/webview2),
  [`connectOverCDP`](https://playwright.dev/docs/api/class-browsertype#browser-type-connect-over-cdp)
- CrabNebula DevTools: [repository](https://github.com/crabnebula-dev/devtools),
  [`devtools/src/lib.rs`](https://github.com/crabnebula-dev/devtools/blob/main/crates/devtools/src/lib.rs),
  [`devtools-core/src/server.rs`](https://github.com/crabnebula-dev/devtools/blob/main/crates/devtools-core/src/server.rs)
- Victauri: [Security](./security.md), [Compatibility](./compatibility.md),
  [Tools Reference](./tools-reference.md)
