# Victauri — Claude Code Instructions

## What Is Victauri

**Victauri — Verified Introspection & Control for Tauri Applications.**

X-ray vision and hands for AI agents inside Tauri apps. Unlike Playwright (which sees only the browser glass), Victauri gives agents simultaneous access to the webview DOM, the Rust backend, the IPC layer, the database, and native window state — all through a single MCP interface.

**Stack:** Pure Rust workspace (7 crates) | **Target:** Tauri 2.0 applications + any website via Chrome/Firefox extension

## Commands

```bash
cargo build --workspace                               # Build all crates
cargo test --workspace                                # Run all Rust tests
cd extensions/chrome/tests && npx vitest run           # Run 163 JS bridge tests
cargo bench -p victauri-core                          # Criterion benchmarks (16)
cargo clippy --workspace --all-targets                # Lint (20 enforced lints)
cargo fmt --all -- --check                            # Format check
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps  # Generate docs (zero warnings)
```

### Version Bumping

**Always use the bump script** when changing versions. It updates all 12+ files atomically:

```powershell
.\scripts\bump-version.ps1 0.6.0          # Windows
./scripts/bump-version.sh 0.6.0           # Linux/macOS
.\scripts\bump-version.ps1 0.6.0 -DryRun  # Preview changes
```

After running the script, manually update:
1. **CHANGELOG.md** — Add release notes under `## [X.Y.Z] - YYYY-MM-DD`
2. **MIGRATION.md** — Add section if there are breaking/behavior changes
3. **CLAUDE.md** — Update Current State date, version, and new feature descriptions
4. Run tests, clippy, fmt, then commit + push + publish

## Architecture

```
victauri/
├── crates/
│   ├── victauri-browser/    # Chrome extension native host: MCP for any website
│   ├── victauri-cli/        # CLI: init, check, test, record, doctor, watch, invoke, coverage
│   ├── victauri-core/       # Shared types: events, registry, snapshots, verification
│   ├── victauri-macros/     # Proc macros: #[inspectable] for command instrumentation
│   ├── victauri-plugin/     # Tauri plugin: embedded MCP server + JS bridge + tools
│   ├── victauri-test/       # Test client + assertion helpers + smoke suite
│   └── victauri-watchdog/   # Crash-recovery sidecar (monitors plugin health)
├── extensions/
│   ├── chrome/              # Chrome/Edge/Brave extension (MV3) + 163 vitest tests
│   ├── firefox/             # Firefox extension (MV3) — browser.* namespace port
│   └── npm/                 # npm package: @4da-systems/victauri-browser (binary installer)
├── docs/                    # mdbook documentation site (10 pages)
└── examples/
    └── demo-app/            # Multi-window Tauri app with comprehensive test suite
```

### How It Works

1. **victauri-plugin** is added as a dev dependency to any Tauri app
2. The plugin starts an axum HTTP server on `127.0.0.1:7373` inside the app process
3. This server speaks MCP protocol (Streamable HTTP + SSE)
4. Claude Code (or any MCP client) connects and gets full-stack control

### The Three Layers

| Layer | What It Does | How |
|---|---|---|
| **WebView** | DOM snapshots, click, type, fill, eval JS | Injected JS bridge via `on_webview_ready()` |
| **IPC** | Command registry, invoke commands, intercept IPC log | Custom invoke handler wrapper + proc macros |
| **Backend** | State reading, DB queries, memory tracking | Direct `AppHandle` access (same process) |

## Crate Responsibilities

### victauri-browser
Native messaging host binary + Chrome extension for browser MCP inspection.
- `main.rs` — CLI (install/uninstall/serve/version), native message reader loop, port fallback :7474-7484
- `native_messaging.rs` — Chrome native messaging wire protocol (32-bit LE length prefix + UTF-8 JSON)
- `bridge_dispatch.rs` — UUID command dispatch via oneshot channels, 30s timeout, cancel-all on disconnect
- `mcp_handler.rs` — 20-tool router: dispatches through bridge to Chrome extension content script
- `mcp_server.rs` — rmcp `ServerHandler` impl with JSON Schema tool definitions for MCP Streamable HTTP
- `server.rs` — axum router: `/mcp` (MCP protocol), `/api/tools` (REST), `/health`, `/info`
- `auth.rs` — Bearer token auth (constant-time eq), token-bucket rate limiter (Retry-After on 429), DNS rebinding guard, origin guard (URL-parsed), security headers (nosniff, no-store, DENY, CORS null, CSP)
- `tab_state.rs` — Per-tab state tracking (URL, title, bridge ready), active tab resolution
- `installer.rs` — Cross-platform native host manifest registration (Chrome/Edge/Brave/Arc on Win/Mac/Linux)
- Chrome extension: MV3 service worker (native messaging + tab lifecycle + CDP + screenshot), ISOLATED world relay, MAIN world JS bridge (1700+ lines — DOM, interactions, a11y, perf, CSS, recording), dark popup UI
- Firefox extension: Full MV3 port using `browser.*` namespace, background scripts (not service workers), no CDP
- npm package: `@4da-systems/victauri-browser` with postinstall binary download from GitHub releases
- 99 Rust tests (5 native_messaging + 4 bridge_dispatch + 6 mcp_handler + 6 mcp_server + 12 server integration + 5 tab_state + 4 installer + 3 auth + 2 router + 52 E2E pipeline)
- 163 JS tests (vitest + jsdom): find-elements (28), interactions (20), helpers (16), dom-snapshot (14), logs (13), css-inspect (12), storage (10), eval (10), performance (9), recording (8), service-worker (8), content-isolated (8), waitfor (7)

### victauri-core
Shared types used by all other crates. No Tauri dependency.
- `EventLog` — append-only ring buffer of `AppEvent` variants (Ipc, StateChange, DomMutation, DomInteraction, WindowEvent, Console)
- `AppEvent::is_internal()` — identifies Victauri's own infrastructure events (e.g. `plugin:victauri|*` IPC)
- `CommandRegistry` — thread-safe registry of `CommandInfo` with search
- `DomSnapshot` / `DomElement` — accessible tree with ref handles
- `WindowState` — position, size, visibility, focus state
- `VerificationResult` / `Divergence` — cross-boundary verification output

### victauri-macros
Proc macro crate. Single attribute macro: `#[inspectable]`.
- Generates `<fn>__schema()` companion returning `CommandInfo`
- Designed to sit alongside `#[tauri::command]`
- Zero runtime cost — all code generation is compile-time

### victauri-plugin
The main crate. Tauri plugin + embedded MCP server.
- `init<R: Runtime>()` — plugin entry point, gated behind `#[cfg(debug_assertions)]`
- JS bridge injection (`js_bridge.rs`) — DOM walking, ref map, console hooks
- MCP server (`mcp/`) — axum on :7373, rmcp tools/resources/prompts (split into mod.rs + server.rs + rest.rs + helpers.rs + 8 param sub-modules)
- REST API (`mcp/rest.rs`) — `GET /api/tools` lists tools, `POST /api/tools/{name}` executes any tool via plain JSON (no MCP handshake)
- Tools (`tools.rs`) — Tauri commands for eval, window state, IPC log, registry, memory
- Screenshot (`screenshot.rs`) — platform-native window capture
- Memory (`memory.rs`) — atomic allocation tracking

### victauri-test
Standalone test crate. No Tauri dependency — only reqwest + serde_json.
- `VictauriClient` — typed HTTP client with auto-session lifecycle
- Convenience methods for all common MCP tool calls
- Assertion helpers for DOM, IPC, accessibility, performance, state verification

### victauri-watchdog
Standalone binary. Monitors the MCP server health endpoint.
- Polls `GET /health` every 5 seconds
- Logs warnings on failure, errors after 3 consecutive misses
- Future: configurable recovery actions (restart app, notify agent)

## Principles

1. **Same-process** — the MCP server runs inside the Tauri app, not as a separate process
2. **Zero-cost in release** — everything gated behind `#[cfg(debug_assertions)]`
3. **Full-stack** — webview + IPC + backend + DB, not just DOM
4. **MCP-native** — speaks the protocol AI agents already understand
5. **Cross-platform** — no CDP dependency, works on Windows/macOS/Linux identically
6. **Plugin, not framework** — one line in Cargo.toml to add, one line to remove

## Design Decisions

- **Why embedded, not external?** Eliminates the three-hop state drift that plagues Playwright. Direct AppHandle access gives sub-ms tool response times.
- **Why axum, not stdio?** Tauri apps are GUI processes — stdin/stdout aren't wired for MCP. HTTP/SSE on localhost is the right transport for an already-running process.
- **Why ref handles, not selectors?** Following Playwright MCP's proven model. Refs are semantic (ARIA-derived), short-lived, and survive DOM restructuring within a snapshot.
- **Why a watchdog?** If the app crashes, the embedded MCP server dies. The watchdog detects this and can alert the agent or trigger recovery.

## Code Conventions

- **Rust:** snake_case functions, PascalCase types, `thiserror` for errors, `anyhow` for application errors
- **Files:** snake_case for Rust
- **No unwrap/panic in library code** — use `?` and `Result` everywhere
- **Imports:** std > external crates > workspace crates > local modules

## Phase Roadmap

### Phase 1: Foundation (Complete)
- [x] Workspace structure
- [x] Core types (events, registry, snapshots)
- [x] Proc macro skeleton (#[inspectable])
- [x] Plugin skeleton (setup, JS bridge injection, axum server)
- [x] Basic tools (eval, window state, IPC log, registry, memory)
- [x] Wire up rmcp MCP server with full tool definitions (11 tools)
- [x] Implement eval-with-return (oneshot channel callback pattern)
- [x] Platform screenshot (Windows PrintWindow → PNG)
- [x] Unit tests (10 core type tests, 3 proc macro tests)
- [x] Fix proc macro bug (type extraction via quote)
- [x] Demo app (minimal Tauri 2 app in examples/demo-app)

### Phase 2: Dual-Context Verification (Complete)
- [x] Cross-boundary state verification tool
- [x] Ghost command detection
- [x] IPC round-trip integrity checking

### Phase 3: Reactive Streaming (Complete)
- [x] MCP resource subscriptions (ipc-log, windows, state)
- [x] Push notifications on state change
- [x] Event stream filtering

### Phase 4: Intent Layer (Complete)
- [x] Command-level intent annotations
- [x] Natural language → command resolution
- [x] Semantic test assertions

### Phase 5: Time-Travel (Complete)
- [x] IPC event recording
- [x] State snapshot checkpointing
- [x] Rewind/replay tools

### Phase 6-7: Expansion (Complete)
- [x] IPC interception (network log derivation)
- [x] Network monitoring, storage, navigation, dialogs
- [x] Window management, wait_for conditions

### Phase 8: Deep Introspection (Complete)
- [x] CSS/style inspection (computed styles, bounding boxes with box model)
- [x] Visual debug overlays (highlight elements with labels)
- [x] CSS injection (inject/remove custom styles)
- [x] Accessibility auditing (WCAG checks: alt text, labels, contrast, ARIA, headings)
- [x] Performance profiling (navigation timing, resource loading, JS heap, long tasks, DOM stats)

## Current State (2026-06-02)

### Agent first-contact reliability — bridge by identity, survive restarts (v0.7.4)

Agents were falling back to CDP because the MCP path could bind the WRONG app (a static
`.mcp.json` `url:` hardcodes a port; `try_bind` silently falls off a busy 7373, so the agent
talked to a stray Demo on 7373 while the real app ran on 7374 — every call 422'd). A static URL
can never guarantee the right port; only a discovery-aware connection can. Fixed by hardening the
`victauri bridge` stdio proxy (the connection `victauri init` already writes):

- **Identity selection.** Discovery `metadata.json` now records the Tauri `identifier` +
  `product_name`. `victauri bridge --app <identifier>` (or `VICTAURI_APP`) binds that exact app
  regardless of which port it landed on; with no selector it uses the single running app, or
  errors clearly listing the running apps — never a silent wrong-app binding.
- **Restart recovery.** The bridge caches the `initialize` handshake and transparently
  re-establishes a fresh session (re-discovering the port) on a stale session (404/409/422) or
  connection drop. The old code cleared the session then replayed the tool call with no session,
  which the server 422'd.
- **First-contact verification.** `/info` and `get_plugin_info` report `app.identifier`.
- **Zero-config:** `victauri init` bakes `--app <identifier>` into `.mcp.json` (read from
  `tauri.conf.json`); the generated CLAUDE.md teaches agents to verify identity, pin `--app`, and
  use the sessionless REST API — not CDP — on any wedge. README/getting-started recommend the
  bridge over a fixed `url:`.
- Proven by an E2E test that drives the real `victauri bridge` binary through a simulated restart
  (selects by identity → forwards → recovers transparently). Full gate green: 1973 workspace tests,
  clippy 0 warnings, `cargo doc -D warnings` clean.

## Current State (2026-05-31)

### Animation-debugging suite — motion introspection, no CDP (2026-05-31, branch `feat/animation-introspection`)

Motion was the last blind spot in agent perception: agents get frozen screenshots and cannot perceive
time-based behaviour. New compound `animation` tool gives an agent quantitative, deterministic,
cross-platform access to the webview's animation engine via the Web Animations API — works identically
on WebView2/WKWebView/WebKitGTK, **no CDP**. **34 MCP tools** now (added `animation`). All three actions
verified live against the demo-app's deliberately-broken sweep.

- **`animation list`** — reads `getAnimations()`: declared `timing` (duration/delay/easing/iterations),
  `computed` progress, `keyframes`, `play_state`, and the animating `target`. Answers "what is the engine
  actually running" in one call. An animation only appears while running/pending, so trigger it first.
- **`animation scrub`** (the differentiator) — pauses the target's WAAPI animation and seeks it to N
  evenly-spaced points (`scrubPrepare`/`scrubSeek`/`scrubRestore` bridge methods; `await animation.ready`
  + double-rAF freezes each frame). Returns the exact geometry curve (rect + transform tx/ty/sx + opacity
  per point); with `capture=true` also returns a single contact-sheet **filmstrip PNG** (one image of the
  whole arc) + a `manifest` mapping cells to progress/time. Frozen frames are jank-free, so the slow
  native screenshot has nothing to race — this beats real-time capture for fast sweeps. CSS-driven
  animations only (JS/rAF animations are not seekable — documented honestly; use `list`/`sample`).
- **`animation sample`** — real-time rAF motion recorder, decoupled from the blocking eval so
  event-triggered sweeps are catchable: `record=true` arms a watcher, trigger the animation, then
  `record=false` reads the measured per-frame curve + jank stats (dropped frames, max frame gap) and
  declared-vs-measured duration. Works for ANY animation including JS/rAF-driven.
- **Filmstrip compositor** (`filmstrip.rs`) — composes raw RGBA frames into one grid PNG (pure Rust,
  7 unit tests). `screenshot.rs` refactored to expose `capture_window_raw` (raw RGBA + dims) on
  Windows/macOS/Linux-X11; Wayland (grim PNG-only) returns a clear error for raw capture.
- **Verified live (demo-app, 2026-05-31):** `list` read the broken config exactly (sweepBroken, 1200ms,
  translateX(420px)→translateX(-48px), overshoot bezier). `scrub` (6 pts, capture) returned the curve
  tx 420→473(backward overshoot)→…→−48 (48px past target) + a 2716×1212 filmstrip. `sample` recorded 145
  frames over measured 1199.8ms, 0 jank, 9.3ms max gap. PrintWindow+PW_RENDERFULLCONTENT captured the
  GPU-composited webview frames correctly. New `VictauriClient` methods: `animation_list`,
  `animation_scrub`, `animation_sample_arm`/`_read`. demo-app has a re-triggerable broken sweep
  (`#sweep-toast`/`#sweep-btn`); agent-eval corpus has task **T7** (calibrate the sweep).

- **`window introspectability`** (new action, same release) — probes every window's bridge and reports
  introspectable vs blind. A visible window that comes back `introspectable:false` is almost always
  missing `victauri:default` in its capability file; the note names the exact file to edit + that a
  rebuild is needed. Motivated by a real 4DA incident: the notification window had no `victauri:default`,
  so Victauri (and eval_js) were silently blind to it while CDP — which sits below Tauri's ACL at the
  WebView2 engine layer — could attach. Victauri is capability-gated because it's a Tauri plugin above
  the IPC boundary; this diagnostic makes that requirement loud + one-line to fix instead of a silent
  timeout. Verified live (demo-app): flags a capability-stripped `main` correctly, passes a capable one.

  Remaining frontier (honest non-goals): JS/rAF (non-WAAPI) animations aren't seekable (scrub errors
  clearly, suggests sample); catching an event-triggered animation at t=0 needs trigger-then-call timing;
  Wayland real-time pixel capture is full-screen via grim; no mp4/h264 export (filmstrip is the
  agent-friendly output). Shipped in **v0.7.4** (additive; stays in `^0.7` so `"0.7"` consumers like
  4DA pick it up without a requirement change — chose 0.7.2 over 0.8.0 deliberately for that reason).

## Current State (2026-05-30)

### Webview Playwright-parity build-out — no CDP (2026-05-30, branch `feat/webview-parity`)

Strategic decision: pursue webview parity WITHOUT CDP (pure JS-bridge + thin OS-native shims). CDP only exists on WebView2 (1 of 3 Tauri webviews) so it can't deliver the cross-platform uniformity that is Victauri's differentiator; CDP-only features (coverage/throttle) matter least for app testing. Each phase verified live. **33 MCP tools** now (added `route`, `trace`).

- **Phase 1 — `route` tool (network interception).** Playwright `route()` equivalent in the JS bridge: match fetch/XHR by URL (substring/glob/regex/exact + method), then `block`/`fulfill` (mock status/headers/body)/`delay`. `times` cap, `matches` log, page-scoped rules. Fetch: all behaviors; XHR: block/delay (fulfill is fetch-only). Not intercepted: top-level navigation, sub-resources, WebSocket. For IPC-layer faults use `fault`.
- **Phase 2 — trusted (OS-level) input.** `input`/`interact` accept `trusted: true` → real OS events (`isTrusted: true`) via Win32 `SendInput` (Unicode keys, named keys, DPI-aware absolute clicks). New `WebviewBridge::native_type_text`/`native_key`/`native_click` with macOS/Linux honest "not implemented" stubs (fall back to synthetic). Verified Windows: keydown/click `isTrusted === true`. Cookie-set: non-httpOnly via `eval_js` `document.cookie`; httpOnly platform-store deferred.
- **Phase 3 — same-origin iframe traversal.** `dom_snapshot`/`find_elements`/`interact`/`input` descend into same-origin frames (cross-origin marked + skipped). Actionability is frame-aware (occlusion/viewport checks run against the element's own document/window).
- **Phase 4 — `trace` tool (screencast).** Background window capture into a ring buffer (`interval_ms`/`max_frames`) via the native screenshot path; `with_events` also drives the recorder. `stop` summary + `frames` (base64 PNGs). Pairs with `recording`+`logs` for a trace bundle. New `screencast` module.

Remaining frontier (documented non-goals without CDP): cross-origin frames, top-level-navigation/sub-resource/WebSocket interception, JS/CSS coverage, CPU throttling, httpOnly cookie-set, native input on macOS/Linux (trait ready, impls pending platform verification).

## Current State (2026-05-29)

### Exhaustive 4DA HEAD test + correctness fixes (2026-05-29)

Tested HEAD exhaustively against live 4DA (real app: 3 windows, 383 commands, 302 MB SQLite DB, 47 MB IPC traffic) via the REST API, then fixed every verified shortfall. All fixes have unit tests and were verified live. See `scripts/e2e/` for the formalized regression harnesses.

- **`eval_js` auto-return rewritten (CRITICAL fix).** The old heuristic prepended `return` to any non-keyword code, so multi-statement blocks (`foo(); return bar()`) silently returned only the first statement's value. Now uses a string/comment/template-aware scan (`should_prepend_return`) that only prepends to a single bare expression. The common "do X then return Y" pattern works correctly.
- **Deep eval results no longer leak the envelope or crash.** Results nested past serde_json's default limit (128) previously leaked the raw `{"__victauri_ok":...}` envelope as a string. `unwrap_eval_envelope` now strips the envelope by string slicing on parse failure (no recursion). The recursion limit is intentionally NOT disabled — that overflows the worker thread stack on pathological depth and crashes the host.
- **Log tools survive busy apps.** `logs ipc`/`network`/`slow_ipc` and `detect_ghost_commands` previously fetched the full IPC/network log (with bodies) and blew the 5 MB eval cap on real apps. Now: default entry limit (100), per-entry field truncation (4 KB, `trimmed_log_js`), and `detect_ghost_commands` projects command names only.
- **`VictauriBuilder::db_search_paths([...])`** lets `query_db`/`introspect db_health` reach databases outside the OS app-data dir (e.g. an app's project/working dir). Configured roots win auto-discovery; absolute paths allowed only within an allowed root. 4DA registers `../data` so its real DB is reachable.
- **`query_db` blocks the write form of PRAGMA** (`PRAGMA x = y`) explicitly (connection was already READ_ONLY).
- **Minor:** `window get_state` on an unknown label errors (was `[]`); `window resize` rejects zero dimensions; `eval_js` timeout message explains the syntax-error case.
- **Known limitation (not safely fixable):** a JS *syntax error* in eval'd code surfaces only as the 30 s timeout — WebView2 does not fire a `window` error event for eval parse errors, and a Rust-side syntax heuristic risks false-positives on valid code (regex/strings). Documented in the timeout message.
- **Parity gaps vs CDP/Playwright (by design):** synthetic events (`isTrusted:false`); no network interception/mock/block (passive logging only); no cookie set; no iframe traversal; no JS/CSS coverage, tracing, throttling, file upload/download, or multi-tab.

## Current State (2026-05-28)

**All 8 phases complete + production hardening + adversarial audit + comprehensive security hardening + REST API + VS Code extension + ultimate compatibility testing (5 third-party apps, 867/895 pass across 179 tests each = 96.9%). v0.7.4. Full browser extension ecosystem (Chrome + Firefox + npm package). CI/CD with release workflow + cross-platform E2E. Documentation site.** 1862+ Rust tests (workspace) + 163 JavaScript tests (Chrome extension vitest). All 7 crates compile cleanly (`RUSTFLAGS="-Dwarnings" cargo clippy` passes). Zero clippy warnings (`-D warnings`, 20 enforced lints). 26 runnable doc-test examples. 16 Criterion benchmarks. CI green on Linux/Windows/macOS + Chrome extension test job + cross-platform E2E job. Tauri 2.10.3 + rmcp 1.5.0. All 7 crates published to crates.io. `cargo install victauri-cli` provides standalone `victauri` binary. Dual-protocol: MCP on `/mcp` + REST on `/api/tools`. VS Code extension in `editors/vscode/`. Chrome extension in `extensions/chrome/` with MV3, 20 MCP tools, native messaging host on :7474. Firefox extension in `extensions/firefox/` (full MV3 port). npm package in `extensions/npm/` with postinstall binary download. mdbook documentation site in `docs/`. GitHub Actions release workflow (cross-platform matrix builds → GitHub Release + crates.io publish + Chrome extension zip). `invoke_command` surfaces Tauri errors (no longer swallows). `find_elements` accepts `selector` as alias for `css` param, returns error for invalid selectors. `eval_js` returns MCP isError for JS exceptions via `__victauri_ok`/`__victauri_err` envelope protocol. Hidden window eval fails fast (2s probe) with bridge ready signal on init. Recording replay/export works after stop. Explain narrative filters Victauri internal traffic via `AppEvent::is_internal()`. `AppEvent::Console` variant for typed console log events. Discovery directory always contains session token for zero-config auth. Soak test (120s) and concurrent stress test (10 clients, 60s) available. 8 regression E2E tests validate all v0.5.3/v0.5.4 fixes. **Security hardening (v0.7.4):** auth-on-by-default with auto-generated UUID v4 tokens, DNS rebinding guard, origin guard with URL-parsed validation, security response headers (nosniff/no-store/DENY/CSP), rate limiter Retry-After, SQL comment stripping + stacked query blocking, discovery file ACLs (icacls/chmod), env var prefix trimming, eval output size limit (5 MB).

### Live test results (4DA, 2026-04-26):
Tested against 4DA (3 windows: main 1200×800, notification 440×160, briefing 560×780; 135 DOM elements; 11 buttons; React/Vite frontend on :4444). **99/99 tests pass — all 23 tools + 3 resources + tool registration checks.**

**WebView tools:**
- **eval_js**: `document.title` → `"4DA"`, `typeof __VICTAURI__` → `"object"`, complex `JSON.stringify({url, keys})` → URL + 12 bridge methods. Auto-return prepend verified. Window targeting (`webview_label:"main"`) works.
- **dom_snapshot**: Full accessible tree with ref handles, element bounds, roles, names. 1192×800 body viewport. Refs survive across interactions.
- **click**: `ref_id:"e3"` → `{ok:true}`. URL updated to `#main-content` confirming UI interaction.
- **fill**: Returns error on non-input elements (correct — e3 is a div, no input elements on current page). Fix applied: handles textarea prototype + fallback.
- **type_text**: `ref_id:"e3"` → `{ok:true}`. Dispatches keydown/keypress/input/keyup events.
- **press_key** [NEW]: Tab → `{ok:true}`, Escape → `{ok:true}`, Enter → `{ok:true}`, ArrowDown → `{ok:true}`, F5 → `{ok:true}`.

**Window tools:**
- **list_windows**: `["notification","briefing","main"]` — all 3 windows.
- **get_window_state**: main (1200×800, visible, `http://localhost:4444/#main-content`), notification (440×160, hidden), briefing (560×780, hidden). Full position/size/visibility/focus/URL data.
- **screenshot** [NEW]: Returns valid base64 PNG (`iVBORw0KGgo...`) via `PrintWindow`+`GetDIBits`. Both default and `window_label:"main"` work.

**Backend tools:**
- **invoke_command** [NEW]: `get_settings` → full 4DA settings JSON (license tier, LLM config, rerank settings, monitoring). `get_monitoring_status` → live monitoring state (enabled, interval, last check timestamp). `get_license_status` → `{}`. Works with args: `search_context` with query param. Nonexistent commands return `{}` (Tauri behavior).
- **get_ipc_log**: Returns full IPC call history derived from fetch interception of `http://ipc.localhost/<command>`. Shows `get_privacy_config`, `get_settings`, `get_monitoring_status`, etc. with timestamps, status, and duration. Limit parameter works.
- **get_registry**: `[]` (4DA doesn't use `#[inspectable]`).
- **get_memory_stats** [NEW IMPL]: Real OS process memory — `working_set_bytes: 77MB`, `peak_working_set_bytes: 290MB`, `page_fault_count: 450K`, `page_file_bytes: 26MB`.
- **get_console_logs** [NEW]: Captures React DevTools message + i18next message with timestamps. `since` filter works.

**Verification tools:**
- **verify_state**: JSON comparison `{title:"4DA"}` → `passed:true, divergences:[]`. Detects divergence when backend_state mismatches (`"Wrong Title"` → `passed:false` with Error severity).
- **detect_ghost_commands**: Finds real ghost commands (e.g. `ace_get_active_topics`, `ace_get_anti_topics`) — frontend-invoked commands not in `#[inspectable]` registry. Works against live IPC data.
- **check_ipc_integrity**: `healthy:true, total_calls:108, pending:0, stale:0, errored:0`. Real integrity checking against live IPC traffic.

**Streaming tools:**
- **get_event_stream**: Returns combined console+DOM mutation events with timestamps. Previously broken (getEventStream undefined in bridge) — **fixed**: deferred MutationObserver init until DOM ready.

**Intent tools:**
- **resolve_command**: `"show settings"` and `"increase counter"` → `[]` (4DA has no `#[inspectable]` commands registered). Correct behavior.
- **assert_semantic**: `expression:"document.title", condition:"equals", expected:"4DA"` → `passed:true, actual:"4DA"`. `expression:"document.querySelectorAll('nav').length", condition:"truthy"` → `passed:true, actual:1`.

**Time-travel tools:**
- **start_recording** → `session_id` UUID, `started:true`.
- **checkpoint** → `checkpoint_id`, `created:true`, `event_index:0`. Label supported.
- **list_checkpoints** → array with id, label, timestamp, state, event_index.
- **get_recorded_events** → events array. **get_replay_sequence** → IPC events only.
- **events_between_checkpoints** → events between named checkpoints.
- **stop_recording** → full session with events + checkpoints.

**Resources:**
- `victauri://state` → `{commands_registered:0, events_captured:0, memory:{working_set_bytes:...}, port:7373}`.
- `victauri://windows` → JSON array of all 3 window states.
- `victauri://ipc-log` → `[]`.

**Health/Info:**
- `/health` → `ok` (no auth required).
- `/info` → `{name:"victauri", port:7373, protocol:"mcp", version:"0.1.0", auth_required:true, commands_registered:0, events_captured:0}`.

**Phase 8: Deep Introspection tools:**
- **get_styles**: Full computed CSS for any element — returns key properties by default or specific properties on request. Returns `display`, `position`, `font-family`, `color`, `width`, `height`, etc.
- **get_bounding_boxes**: Precise pixel measurements with CSS box model (margin, padding, border) for multiple elements at once.
- **highlight_element**: Draws colored overlay with optional label on any element for visual debugging. Fixed-position, non-interactive, z-index:max.
- **clear_highlights**: Removes all debug overlays.
- **inject_css**: Injects custom CSS into the page for debugging/prototyping. Replaces previous injection.
- **remove_injected_css**: Removes injected CSS.
- **audit_accessibility**: Comprehensive a11y audit — checks images without alt text, unlabeled form inputs, empty buttons/links, heading hierarchy, color contrast (WCAG AA), ARIA role validity, positive tabindex, missing document language/title. Returns violations + warnings with severity levels and summary counts.
- **get_performance_metrics**: Navigation timing (DNS, TTFB, DOM interactive/complete, load event), resource summary (count, transfer size, by type, 5 slowest), paint timing (FP, FCP), JS heap usage (used/total/limit MB), long task count, DOM stats (element count, max depth, event listener count).

**Bridge methods:** version, snapshot, getRef, click, fill, type, pressKey, getConsoleLogs, clearConsoleLogs, getMutationLog, clearMutationLog, getEventStream, getStyles, getBoundingBoxes, highlightElement, clearHighlights, injectCss, removeInjectedCss, auditAccessibility, getPerformanceMetrics.

### Real-app compatibility testing (2026-05-14):
Tested against 4 third-party open-source Tauri 2 apps with fully built frontends.

**Smoke tests (24 per app): 96/96 pass — zero failures.**

| App | Framework | Elements | JS Heap | Window Size | A11y Violations |
|-----|-----------|----------|---------|-------------|-----------------|
| **Kanri** (kanban board) | Nuxt 4 / Vue 3 / TailwindCSS | 234 | 6.19 MB | 1400×800 | 8 |
| **En Croissant** (chess) | React / TanStack Router / Mantine | 201 | 18.17 MB | 800×631 | 9 |
| **Duckling** (database explorer) | React 19 / Jotai / TailwindCSS 4 | 301 | 76.45 MB | 1000×800 | 8 |
| **Lettura** (RSS reader) | React / PWA / Custom UI | 109 | 8.31 MB | 1440×740 | 1 |

**Deep functional tests (71 per app): 266/275 pass across all 4 apps (96.7%).**

| App | Pass | Fail | Notes |
|-----|------|------|-------|
| **Kanri** | 70/71 | 1 | Test script false positive (recording stop response parsing) |
| **En Croissant** | 63/66 | 3 | 2 test script regex misses (no named buttons/headings in DOM), 1 recording FP |
| **Duckling** | 67/71 | 4 | 3 actionability (Jotai Devtools overlay covers button after click), 1 recording FP |
| **Lettura** | 66/71 | 5 | 4 actionability (Today button has `pointer-events:none`), 1 recording FP |

**All "failures" are either test script issues or correct Playwright-grade actionability enforcement — zero Victauri bugs.**

**Deep test coverage (14 phases, 71 tests each):**
1. **DOM & Find** — snapshot tree, find_elements by selector (button/a/input/img)
2. **Interaction** — click, hover, focus, scroll_into_view, double_click with actionability checks
3. **Input** — fill (set value), type_text (character-by-character), press_key (Tab/Escape/Enter/ArrowDown)
4. **Style inspection** — computed styles (display, color, font-size, font-weight), specific properties, bounding boxes with CSS box model
5. **Visual debug** — highlight element with color/label overlay, screenshot with overlay, clear highlights, CSS injection/removal
6. **Window management** — get_state, set_title (verified roundtrip), resize (verified), move_to, minimize/unminimize
7. **Storage** — localStorage set/get/delete with verification, get_cookies
8. **Navigation** — current URL, history log, dialog log, go_back
9. **Semantic assertions** — equals/contains/greater_than conditions with JS expressions, correct failure detection
10. **Cross-boundary verification** — frontend_expr vs backend_state match/mismatch with divergence detection
11. **Wait for** — selector exists, URL contains, selector_gone, timeout detection
12. **Time-travel recording** — start session, checkpoint with ID/label, list checkpoints, get events, stop with full session export
13. **Logs** — console (with entry counts), network (57-106 entries per app), IPC, navigation, dialogs, events, slow_ipc
14. **Complex eval & backend** — JSON return, async Promise resolution, heavy computation (1M iterations), memory stats, plugin info, diagnostics, registry

**Actionability checks confirmed working:**
- Kanri: modal backdrop (`div.backdrop-brightness-50`) correctly blocks click/hover/scroll on covered elements
- Duckling: Jotai Devtools overlay correctly blocks interaction after click opens devtools
- Lettura: `pointer-events:none` on icon buttons correctly detected and rejected

**Key compatibility findings:**
- Works across Vue 3 (Nuxt), React 18, React 19, PWA — framework-agnostic as designed.
- `victauri:default` capability must be added to the app's capabilities JSON or IPC callbacks silently fail (Tauri permission system blocks with no error).
- Debug binaries embed `frontendDist` at compile time — frontend must be built BEFORE `cargo build`. Running debug binary directly uses embedded files, not `devUrl`.
- Apps with `devUrl` configured will use the live dev server if running, otherwise fall back to embedded `frontendDist` files.
- All 4 apps required zero Victauri code changes — plugin integration is purely additive (1 line Cargo.toml + 1 line plugin init + 1 line capabilities).

### Ultimate test suite (2026-05-14):
179 tests per app across 18 modules. 5 third-party apps tested: Kanri (Vue/Nuxt, kanban), En Croissant (React, chess), Surrealist (React 19/Mantine, SurrealDB IDE), Duckling (React 19/Jotai, database explorer), Lettura (React, RSS reader).

**867/895 tests pass across all 5 apps (96.9%).**

| App | Framework | Stars | Pass | Fail | Rate | Duration |
|-----|-----------|-------|------|------|------|----------|
| **Kanri** | Nuxt 3 / Vue 3 | 1.1k | 174/179 | 5 | **97.2%** | 21.5s |
| **En Croissant** | React / Mantine | 1.2k | 177/179 | 2 | **98.9%** | 20.8s |
| **Surrealist** | React 19 / Mantine / CodeMirror | 3.0k | 176/179 | 3 | **98.3%** | 24.9s |
| **Duckling** | React 19 / Jotai / TreeSitter | 0.7k | 169/179 | 10 | **94.4%** | 61.1s |
| **Lettura** | React / Custom UI | 1.6k | 171/179 | 8 | **95.5%** | 56.4s |

**18 test modules (179 tests total):**
1. **Server Infrastructure** (11) — health, info, tool listing, auth enforcement (correct token/wrong token/no token), rate limiting burst, plugin info, memory stats, diagnostics, registry, concurrent health
2. **JS Bridge & Eval Engine** (15) — bridge detection, version, method enumeration, arithmetic, document.title, string ops, JSON roundtrip, async/await, heavy computation (1M iterations), error handling, DOM access, window properties, multi-statement, computed style, performance timing
3. **DOM Tree & Element Finding** (12) — snapshot, ref count, find by selector (button/a/input/img/heading/aria role), ref stability, element count, text search, max nesting depth
4. **Interaction Engine** (18) — click (multiple buttons), hover, focus, scroll_into_view, double_click, fill, type_text, clear + refill, 8 keyboard keys (Tab/Escape/Enter/Arrow*/F5), invalid ref handling
5. **CSS Inspection & Visual Debug** (14) — computed styles (all + specific + box model + layout), bounding boxes (single + multiple), highlight with color/label, screenshot with highlight, multi-highlight, clear highlights, CSS injection, screenshot with CSS, CSS removal
6. **Window Management** (14) — list windows, get_state (all fields), set_title + verify, resize + verify, move_to + verify, minimize/unminimize, maximize/unmaximize, state restoration
7. **Screenshot Engine** (6) — basic capture, PNG header validation, size check, targeted window, diff detection (before/after UI change), timing
8. **Storage** (8) — set/get/delete cycle with verification, numeric values, cookie access, missing key handling
9. **Navigation** (6) — current URL, history, dialog log, URL protocol check, hash navigation
10. **Semantic Assertions** (10) — equals, not_equals, contains, greater_than, less_than, intentional failures (verify false detection), viewport width
11. **Cross-Boundary Verification** (8) — bridge match/mismatch, title match, URL protocol, IPC integrity, ghost commands, multi-field verification, nested object verification
12. **Wait For Conditions** (8) — selector exists (body/div), selector_gone, text match, text_gone, URL match, timeout detection, complex selector
13. **Time-Travel Recording** (12) — start, generate events, checkpoint with label, second checkpoint, list checkpoints, get events, events between checkpoints, replay sequence, export, stop with session data, restart, clean state after stop
14. **Logging System** (10) — generate known entries (log/warn/error), console capture verification, network log, IPC log, navigation log, dialog log, events, slow IPC, console with time filter
15. **Accessibility Audit** (6) — audit run, violations, warnings, violation types, contrast check, image alt check
16. **Performance Profiling** (8) — DOM stats, JS heap, heap usage %, navigation timing, paint timing, resources, long tasks, eval latency
17. **Stress & Edge Cases** (10) — rapid-fire 50 evals, large string (10K chars), deep object nesting, unicode/emoji, null/undefined, empty string, 10 concurrent eval calls, rapid DOM snapshots (10x), invalid params, empty params
18. **Tool Orchestration** (6) — snapshot→click→snapshot pipeline, record+interact+verify workflow, memory before/after tracking, verify→assert→screenshot pipeline, a11y+perf pipeline, total invocation count

**All 28 "failures" are either:**
- DOM ref instability between snapshots (DOM changes on interaction — correct behavior)
- Actionability enforcement (Playwright-grade checks correctly blocking covered/hidden/pointer-events:none elements)
- Recording assertion strictness (events captured correctly, just assertion too strict)
- Window label mismatch (Surrealist uses dynamic window labels, not "main")

**Zero Victauri bugs. Zero framework-specific issues. All 5 apps integrated with zero code changes.**

**Additional apps attempted:**
- **GitButler** (SvelteKit, 20.7k stars): SvelteKit monorepo build requires full pipeline — frontend build fails from shallow clone
- **Clash Verge Rev** (React, 8.6k stars): Requires sidecar binaries (verge-mihomo) not included in source
- **DevTools-X** (React/Mantine): Pre-existing image crate version conflict unrelated to Victauri

### What exists and works:
- **victauri-core**: `EventLog` (ring buffer), `CommandRegistry` (BTreeMap with search + NL resolve), `DomSnapshot`, `WindowState`, `VerificationResult`/`Divergence`, `GhostCommandReport`, `IpcIntegrityReport`, `SemanticAssertion`/`AssertionResult`, `ScoredCommand`, `EventRecorder` (time-travel recording with checkpoints), `RecordedSession`, `RecordedEvent`, `StateCheckpoint`. 157 tests (32 codegen unit + 121 core + 4 compile tests). 16 Criterion benchmarks across 5 groups. All mutex/rwlock calls use poisoning recovery.
- **victauri-macros**: `#[inspectable]` proc macro with `description`, `intent`, `category`, `example` attributes. Uses proper `syn::meta` parsing (not string matching). Generates `<fn>__schema()` returning `CommandInfo` with full intent metadata. 4 integration tests.
- **victauri-plugin**: Full MCP server with **31 tools** + 3 resources. Tools organized by category:
  - **Standalone (19)**: eval_js, dom_snapshot, find_elements, invoke_command, screenshot, verify_state, detect_ghost_commands, check_ipc_integrity, wait_for, assert_semantic, resolve_command, get_registry, get_memory_stats, get_plugin_info, get_diagnostics, app_info, list_app_dir, read_app_file, query_db
  - **Compound (12)**: interact (click/hover/focus/scroll/select), input (fill/type/press_key), window (get_state/list/manage/resize/move/set_title), storage (get/set/delete/cookies), navigate (go_to/back/history/dialogs), recording (start/stop/checkpoint/events/export/import/replay), inspect (styles/bounds/highlight/a11y/perf), css (inject/remove), logs (console/network/ipc/navigation/dialogs/events/slow_ipc), **introspect** (command_timings/coverage/contract_record/contract_check/contract_list/contract_clear/startup_timing/capabilities/db_health/plugin_state/processes/plugin_tasks/event_bus/event_bus_clear), **fault** (inject/list/clear/clear_all), **explain** (summary/last_action/diff)
  Resources: victauri://ipc-log, victauri://windows, victauri://state with subscribe/unsubscribe. JS bridge v0.7.4 with IPC interception, network monitoring, storage access, navigation tracking, dialog capture, extended interactions, and waitFor. `EventRecorder` with 50,000 event capacity. **Release-safe**: `init()` returns a no-op plugin in release builds via `#[cfg(debug_assertions)]` gate. `VictauriBuilder` for port/capacity/auth configuration + `VICTAURI_PORT`/`VICTAURI_AUTH_TOKEN` env vars. Bearer token auth middleware (**enabled by default** with auto-generated UUID v4 token, case-insensitive per RFC 7235). `auth_disabled()` to opt out, or `auth_token("...")` for a fixed token. Token-bucket rate limiter (AtomicU64, 1000 req/sec default). Privacy layer with command allowlists/blocklists, tool disabling, regex-based output redaction, strict mode. Tool enable/disable via builder. 203 unit tests + 128 integration tests + 38 adversarial tests + 85 tool contract tests + 30 bridge tests + 22 stress tests + 19 platform tests.
- **victauri-test**: Typed MCP HTTP client (`VictauriClient`) with auto-session management (initialize + notifications/initialized). 23 convenience methods for tool calls (eval_js, dom_snapshot, click, fill, etc). 6 standalone assertion helpers: `assert_json_eq`, `assert_json_truthy`, `assert_no_a11y_violations`, `assert_performance_budget`, `assert_ipc_healthy`, `assert_state_matches`. 11 client assertion methods: `assert_eval_works`, `assert_dom_snapshot_valid`, `assert_screenshot_ok`, `assert_windows_exist`, `assert_ipc_integrity_ok`, `assert_accessible`, `assert_dom_complete_under`, `assert_heap_under_mb`, `assert_no_uncaught_errors`, `assert_recording_lifecycle`, `assert_health_hardened`. Built-in `smoke_test()` suite (11 checks, returns `SmokeReport` with timing + JUnit XML). `SmokeConfig` for custom thresholds. Supports Bearer token auth via `connect_with_token`. Published to crates.io as standalone crate.
- **victauri-cli**: CLI binary (`victauri`) with 8 commands: `init` (scaffold test directory + CLAUDE.md with agent instructions), `check` (server diagnostics), `test` (built-in smoke suite — 11 checks with pass/fail + JUnit XML), `record` (capture interactions → test file), `doctor` (full setup diagnosis), `watch` (file watcher → re-run tests), `invoke` (call any Tauri IPC command from terminal), `coverage` (IPC command coverage report). `victauri test` auto-discovers the running app, runs all smoke checks, prints a summary, exits 0/1 for CI. Configurable `--max-load-ms` and `--max-heap-mb` thresholds. `victauri init` creates/appends CLAUDE.md with instructions that make AI agents prefer Victauri over CDP/Playwright.
- **victauri-watchdog**: Configurable via env vars (`VICTAURI_PORT`, `VICTAURI_INTERVAL`, `VICTAURI_MAX_FAILURES`, `VICTAURI_ON_FAILURE`). Proper `tracing-subscriber` log output. Executes configurable recovery commands on failure. Fires recovery action once per failure cycle, resets on recovery.
- **demo-app**: Multi-window Tauri 2 app in `examples/demo-app/` with Victauri wired up. 19 commands (greet, counter CRUD, todo CRUD, settings, contact form with validation, notifications with cross-window events, window management, app state dump) all decorated with `#[inspectable]`. Tab-based navigation with ARIA attributes, `data-testid` on all interactive elements. Notification panel window with event sync. 20 integration tests in `tests/integration.rs` demonstrating every Victauri testing pattern (direct client API, Locator API, IPC verification, cross-boundary state, a11y audit, perf monitoring, time-travel recording, verify builder). Includes `.mcp.json` for immediate Claude Code connection.
- **CI/CD**: GitHub Actions `ci.yml` (clippy + tests + docs on Linux/Windows/macOS, format check, Chrome extension vitest job) + `release.yml` (test gate → 12-matrix cross-platform builds → Chrome extension zip → sequential crates.io publish → GitHub Release with all artifacts). All crate code passes `cargo fmt --check`.
- **docs/**: mdbook documentation site — 10 pages covering introduction, getting started, architecture, tools reference, Chrome extension, testing, configuration, security, FAQ.

### Architecture notes:
- **victauri-browser architecture** — `MCP Client → axum HTTP :7474 → Native Messaging (stdio) → Chrome Extension Service Worker → Content Script (MAIN world)`. The Rust binary serves dual roles: HTTP server for MCP clients AND native messaging host for Chrome. Both run concurrently via tokio tasks. The `BridgeDispatch` sends UUID-tagged commands to stdout (Chrome native messaging), and a spawned reader task receives responses on stdin and resolves oneshot channels. The `mcp_handler.rs` routes all 20 tools: `get_plugin_info` and `tabs.list` are handled locally in the Rust host; everything else is dispatched to the Chrome extension via native messaging → service worker → content script relay → MAIN world bridge. The content script uses CustomEvents (`__victauri_command`/`__victauri_response`) to bridge ISOLATED ↔ MAIN worlds. Navigation uses `chrome.tabs.update()` instead of content script `window.location`, and cookies use `chrome.cookies.getAll()` for httpOnly access.
- **bridge.rs** — `WebviewBridge` trait (public) erases the Tauri `Runtime` generic, allowing the MCP handler (which can't be generic) to access webview windows AND backend resources via `Arc<dyn WebviewBridge>`. 13 methods: eval_webview, get_window_states, list_window_labels, get_native_handle, manage_window, resize_window, move_window, set_window_title + backend access: app_data_dir, app_config_dir, app_log_dir, app_local_data_dir, tauri_config. Impl provided for `AppHandle<R: Runtime>`. Backend methods have default implementations (return error) so mock bridges work without change. Cross-platform `get_native_handle`: Windows HWND, macOS CGWindowID (via ObjC runtime `windowNumber`), Linux Xlib/Xcb window ID. Testable via mock implementations.
- **introspection.rs** — Backend introspection and chaos engineering types: `CommandTimings` (per-command timing with min/max/avg/p95 stats), `FaultRegistry` (fault injection rules with delay/error/drop/corrupt), `ContractStore` (IPC contract baselines with JSON shape diffing for schema drift detection), `StartupTimeline` (plugin init phase timestamps), `ChildProcessInfo` + `enumerate_child_processes()` (cross-platform child process enumeration via Windows `CreateToolhelp32Snapshot`, Linux `/proc`, macOS `proc_listchildpids`). All state is thread-safe (`RwLock` + poisoning recovery). `JsonShape` recursively extracts type structure from JSON for contract comparison. `diff_shapes()` detects new fields, removed fields, and type changes between baseline and current responses.
- **mcp/** — Split into `mod.rs` (handler + server startup + tests), `server.rs` (Router + server lifecycle), `rest.rs` (REST API routes), `helpers.rs` (js_string, tool_error, validate_url, sanitize_css_color), and 8 param modules (webview, window, backend, verification, recording, introspection, compound, other). rmcp `#[tool_router]` + `#[tool_handler]` macros require all tool methods in a single `impl` block, so the handler stays monolithic. `build_app()` constructs the axum `Router` independently of Tauri (testable). `StreamableHttpService` serves on `/mcp`. REST API on `/api/tools`. Health/info endpoints on `/health` and `/info`.
- **REST API** (`mcp/rest.rs`) — Dual-protocol: all 31 tools accessible via `POST /api/tools/{name}` with plain JSON body, no MCP session needed. `GET /api/tools` lists available tools. Uses the same `VictauriMcpHandler.execute_tool()` dispatch that applies privacy checks, rate limiting, auth, and output redaction. Response format: `{"result": ...}` for success, `{"error": "..."}` for errors. Text results parsed as JSON when valid. Goes through the same auth/rate-limit middleware as MCP.
- **tools.rs** — Tauri commands still work independently for in-app IPC. Both the MCP tools and Tauri commands use the same `pending_evals` mechanism for JS eval with return.
- **screenshot.rs** — Windows: `PrintWindow` → `GetDIBits` (BGRA) → RGBA → PNG. macOS: `CGWindowListCreateImage` → `CGBitmapContext` (RGBA) → PNG. Linux: X11 `GetImage` (BGRA ZPixmap) → RGBA via `x11rb`, with Wayland fallback via `grim` subprocess (full-screen capture). All platforms use the same custom PNG encoder with flate2 zlib compression (CRC32 + Adler32).
- **auth.rs** — Bearer token authentication, **enabled by default** with auto-generated UUID v4 token written to discovery directory. `auth_disabled()` to opt out, or `auth_token("...")` for a fixed token. `VICTAURI_AUTH_TOKEN` env var overrides. `require_auth` axum middleware skips `/health` but protects `/mcp` and `/info`. DNS rebinding guard validates Host header. Origin guard validates Origin header (URL-parsed, blocks subdomain smuggling). Security headers: nosniff, no-store, X-Frame-Options DENY, CORS null, CSP default-src none. Rate limiter returns Retry-After on 429.
- **JS bridge injection** — Uses `js_init_script()` (persistent) instead of `on_webview_ready()` + `eval()` (one-shot). This ensures the bridge survives page navigations in Vite dev mode. MutationObserver init is deferred via `DOMContentLoaded` fallback to avoid crash when `document.documentElement` isn't ready during early script execution. Bridge includes Playwright-grade actionability (10-point checks + auto-wait), stable WeakRef handles, compact accessible-text snapshots, findElements search, full IPC data capture (request args + response body), network interception (fetch + XMLHttpRequest), navigation tracking (pushState/replaceState/popstate/hashchange), dialog capture with configurable auto-responses, waitFor polling, Playwright-style actionability checks (visible, enabled, non-zero size) for click/doubleClick/hover/fill/type, and **global error capture** (`window.onerror` + `unhandledrejection` → consoleLogs with `[uncaught]` prefix). Log caps: consoleLogs 1000, networkLog 1000, navigationLog 200, dialogLog 100.
- **IPC interception** — Tauri 2.0 freezes `__TAURI_INTERNALS__` and all its methods (`invoke`, `ipc`, `postMessage`) with `configurable:false, writable:false`. Plugin init scripts run AFTER Tauri's core init, so monkey-patching is impossible. Instead, IPC is derived from the network log: Tauri sends all IPC via `fetch()` to `http://ipc.localhost/<command>`, and our fetch interceptor captures these. `getIpcLog()` filters networkLog entries for `ipc.localhost` URLs, extracts command names from the URL path, and excludes `plugin:victauri|` calls. This approach is robust against Tauri version changes and works on all platforms.
- **eval auto-return** — `eval_with_return()` auto-prepends `return` to bare expressions (e.g. `document.title` → `return document.title`). Only checks `starts_with("return ")` — NOT `contains("return ")` — so IIFEs with internal returns are handled correctly. Skips statement keywords (`if`, `for`, `const`, etc.).
- **Multi-window safety** — Default window selection prefers "main" → first visible → any, avoiding silent failures when hidden windows lack plugin capabilities.
- **CSP compatibility** — `eval()` cannot be used inside injected scripts when CSP has `script-src 'self'` without `'unsafe-eval'`. The eval wrapper uses direct `(async () => { ... })()` pattern instead.

### Key technical decisions already made:
- MCP server is EMBEDDED in Tauri process (not separate), via axum on `:7373` with **port fallback** (tries :7374-7383 if taken, writes `victauri.port` to temp dir for client discovery)
- `rmcp` v1.5.0 is the MCP SDK, feature `transport-streamable-http-server`
- JS bridge uses ref handles (Playwright pattern), not CSS selectors
- All plugin code gated behind `#[cfg(debug_assertions)]` — `init()` returns no-op plugin in release builds
- OS-level process memory tracking (Windows `GetProcessMemoryInfo`, Linux `/proc/self/statm`) — real metrics, no consumer opt-in needed
- Event log is a `VecDeque` ring buffer with 10,000 capacity
- WebviewBridge trait object pattern for runtime-erased AppHandle access
- `tokio::sync::Mutex` for pending_evals (async lock needed across eval timeout awaits)
- `build_app()` separated from `start_server()` — router construction is testable without Tauri runtime
- IPC data pipeline derives from network log — `get_ipc_log`, `detect_ghost_commands`, `check_ipc_integrity` all call `__VICTAURI__` methods via `eval_with_return()`. IPC entries are extracted from networkLog by filtering `http://ipc.localhost/` URLs (Tauri's fetch-based IPC transport)
- Eval timeout is 30s (not 10s) to support `wait_for` tool's configurable polling timeout
- **Auto-event recording** — background `event_drain_loop` polls `getEventStream()` every 1s while recording is active, converting JS events (console, mutation, IPC, network, navigation) into `AppEvent` variants and feeding them into `EventRecorder`. Time-travel now works automatically without manual tool calls.
- **Port fallback** — `try_bind()` tries preferred port, then +1 through +10. Writes `<temp>/victauri.port` file for client discovery, removes on shutdown. `VictauriState.port` is `AtomicU16` updated to actual bound port.
- **Dual-protocol (MCP + REST)** — REST routes (`/api/tools`) are nested in the axum Router alongside `/mcp`, sharing the same auth/rate-limit/security middleware. `VictauriMcpHandler::execute_tool()` dispatches by tool name, deserializes JSON args into the appropriate param struct, and calls the rmcp `#[tool]` method directly. No MCP session or handshake needed for REST calls.
- **REST is the robust path for agent scripting (not a fallback)** — Verified against live 4DA (2026-05-31): MCP over `/mcp` is **session-stateful** — rmcp's `StreamableHttpService` rejects a tool call with HTTP **422 "expected initialized request"** whenever the session is stale (client reconnect, server restart, missed `notifications/initialized`). REST over `POST /api/tools/{name}` is **sessionless** — every call is self-contained, so it sidesteps that whole failure class while going through the identical auth/rate-limit/privacy/dispatch pipeline (zero capability loss). The only practical delta: REST returns `{"result": ...}`/`{"error": ...}` JSON instead of MCP envelopes, and resource *subscriptions* (push) remain MCP-only. **Guidance: for one-shot tool calls and any scripted/agent-driven loop, prefer REST; reserve MCP for interactive sessions that want live resource subscriptions.** The 422 is an rmcp transport-layer behaviour, not a Victauri bug — REST exists precisely so it never blocks real work. `eval_js` over REST was the workhorse in the 4DA animation investigation.
- **Auth enabled by default** — Auto-generates a UUID v4 Bearer token on startup, written to `<temp>/victauri/<pid>/token`. `VictauriClient::discover()` reads it automatically. `auth_disabled()` to opt out for simple local-only setups. `auth_token("...")` or `VICTAURI_AUTH_TOKEN` env var for fixed tokens. DNS rebinding guard + origin guard + security headers applied to all responses.
- **CLAUDE.md scaffolding** — `victauri init` creates/appends CLAUDE.md with instructions that make AI agents prefer Victauri's 31 MCP tools over CDP/Playwright. This is the highest-leverage fix for agent tool selection — agents read CLAUDE.md before choosing tools.
- **`register_command_names` builder API** — Lightweight alternative to `#[inspectable]` proc macros. Pass `&["cmd1", "cmd2"]` to register commands without schema generation. `commands()` method accepts full `CommandInfo` schemas for rich metadata.
- **`__TAURI_INTERNALS__` not `__TAURI__`** — All eval callbacks and invoke_command use `window.__TAURI_INTERNALS__.invoke()`, NOT `window.__TAURI__.core.invoke()`. `__TAURI_INTERNALS__` is always available regardless of `withGlobalTauri` config. `window.__TAURI__` only exists when the app sets `withGlobalTauri: true`. Discovered via real-world testing against En Croissant (v0.2.1 fix).
- **Fault injection architecture** — The `fault` tool injects rules into `FaultRegistry` (thread-safe `RwLock<HashMap>`). The check lives in the MCP `invoke_command` tool (`mcp/mod.rs:268`), running before that tool executes a command. Fault types: `Delay` (tokio::sleep before execution), `Error` (return error, skip execution), `Drop` (return `{}`), `Corrupt` (execute then mangle response). Trigger counting with optional `max_triggers` limit. **SCOPE (verified 2026-05-31, agent-eval):** faults apply ONLY to commands driven through Victauri's own `invoke_command` tool — they do NOT intercept the app's real frontend IPC (`window.__TAURI_INTERNALS__.invoke`). That path is served below the JS `window.fetch` layer Victauri can reach, so it cannot be blocked/delayed/faulted cross-platform without CDP (confirmed empirically: a `route` block/delay on `ipc.localhost` *matches* the call but does NOT control it — the real `invoke` still succeeds with no delay). So `fault` tests a handler's error path when the agent drives it; it does NOT reproduce a failure a user clicking the UI would experience. Honest framing: this is an *agent-driven backend-handler* fault tool, not live-IPC chaos — and Tauri IPC *control* is actually a place where a CDP-based tool can do more than Victauri.
- **Command profiling** — `invoke_command` records execution duration in `CommandTimings` per command. `introspect.command_timings` aggregates min/max/avg/p95 with optional slow-command threshold filtering. The timing includes the full round-trip: JS eval injection → `__TAURI_INTERNALS__.invoke()` → Tauri IPC → Rust handler → response serialization → JS callback.
- **IPC contract testing** — `contract_record` invokes a command and records the JSON shape (recursive type structure) of the response. `contract_check` re-invokes all baselined commands and diffs against recorded shapes, detecting new fields, removed fields, and type changes. This catches silent IPC breaking changes that tests miss.
- **EventBusMonitor** — `Arc<RwLock<VecDeque<CapturedTauriEvent>>>` ring buffer (1000 capacity) for Tauri native events. Automatic capture of window lifecycle events (focus, blur, resize, close, move, etc.) via `RunEvent::WindowEvent` handler — no app opt-in needed. Custom app events captured via `VictauriBuilder::listen_events(&["event-name"])` which registers `listen_any` handlers. Combined with `EventLog` app events in the `introspect.event_bus` action for a unified event timeline.
- **TaskTracker** — Tracks spawned async tasks (MCP server, event drain loop, on_ready probe) via `Arc<AtomicBool>` finished flags. `introspect.plugin_tasks` reports active/finished counts. Helps agents diagnose background task failures.
- **Plugin state introspection** — `introspect.plugin_state` serializes the full `VictauriState` internals: event counts, registry size, recording state, active faults, contract baselines, timing data, task status, tool invocations, uptime, and port. Answers "what does the plugin know?" in one call.
- **IPC replay** — `recording.replay` re-executes all IPC commands captured during a recording session via `invoke_command`, comparing response shapes. Reports per-command pass/fail with shape diff on drift. Enables regression testing from recorded sessions.
- **Explain tool** — Natural-language narration via `explain` compound tool. `summary` aggregates EventLog events over a time window into a narrative with type counts (IPC, DOM, console, state, window, interaction) and top commands. `last_action` maps events to a causal chain. `diff` counts IPC calls, DOM changes, console messages, errors, and interactions. All use `EventLog.since()` for time-windowed queries with `chrono::TimeDelta`. Internal Victauri events filtered via `AppEvent::is_internal()`.
- **Bridge ready signal** — JS bridge calls `victauri_eval_callback` with ID `__victauri_bridge_ready__` at the end of its IIFE initialization. `VictauriState.bridge_ready` (`AtomicBool`) + `bridge_notify` (`tokio::sync::Notify`) track readiness. `eval_with_return_timeout` waits up to 5s for this signal before first eval, using double-check pattern to close the race window. Eliminates first-call latency from the 2s probe mechanism. Per-window probing still used for explicitly targeted windows.
- **Discovery session tokens** — `start_server()` writes the active auth token to `<temp>/victauri/<pid>/token` (user-only permissions: Unix 0o600, Windows icacls current-user-only). `VictauriClient::discover()` reads the token file and includes it as Bearer header. Zero-config: auth is on, token is auto-discovered, no manual setup needed.
- **`AppEvent::Console` variant** — `#[non_exhaustive]` `AppEvent` enum now has `Console { level, message, timestamp }` instead of mapping console logs to `StateChange { key: "console.warn", caused_by: message }`. Explain handlers count console events separately. `parse_bridge_event()` in `server.rs` creates `Console` variants. Backward-compatible addition (non-exhaustive enum).

### Relationship to 4DA:
Victauri is a standalone open-source project. They share no code. The 4DA repo is at `D:\4DA`, this repo is at `D:\runyourempire\victauri`.

**4DA consumes Victauri as a PUBLISHED crates.io dependency**, NOT a path dep — `victauri-plugin = "0.7"` and `victauri-test = "0.7"` in `src-tauri/Cargo.toml`. This means **4DA only ever sees published features.** As of 2026-05-31 its `src-tauri/Cargo.lock` resolves `victauri-plugin` to **0.7.0**, and crates.io tops out at **0.7.1** — so the animation suite + `window introspectability` (this repo's HEAD `0.7.2`) are **not yet published and 4DA cannot run them**. That is exactly why the 4DA notification-animation work fell back to raw `eval_js` over REST and found the capability gap by hand instead of via the diagnostic.

To test unreleased HEAD tools against 4DA, temporarily point its dep at this repo and revert after:
```toml
# 4DA src-tauri/Cargo.toml — TEST ONLY, revert to victauri-plugin = "0.7"
victauri-plugin = { path = "../../runyourempire/victauri/crates/victauri-plugin" }
```
Then `cargo build` 4DA and relaunch. `victauri:default` is in 4DA's capabilities (`default.json`; `notification.json` + `briefing-window.json` added 2026-05-31 in 4DA commit `381f26f2`) and `.mcp.json` is configured.

### Owner:
4DA Systems Pty Ltd (ACN 696 078 841). Apache-2.0 license. Contact: hello@4da.ai.

## Never Commit
- `target/` — build artifacts
- Any API keys or credentials
