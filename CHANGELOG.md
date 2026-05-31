# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.0] - 2026-05-31

### Added — Animation-debugging suite (motion introspection, no CDP)

- **New `animation` compound tool (34th MCP tool)** — gives an agent quantitative, deterministic, cross-platform access to the webview's animation engine via the Web Animations API. Works identically on WebView2/WKWebView/WebKitGTK with no CDP. Motion was the last blind spot in agent perception (screenshots are frozen instants); this closes it.
  - **`list`** — `getAnimations()` introspection: declared timing (duration/delay/easing/iterations), computed progress, keyframes, play state, and the animating target element. (An animation only appears while running/pending — trigger it first.)
  - **`scrub`** — deterministically pauses the target's animation and seeks it to N evenly-spaced points (`await animation.ready` + double-rAF freezes each frame), returning the exact geometry curve (rect + transform + opacity per point). With `capture=true`, also returns a single contact-sheet **filmstrip PNG** of the whole arc plus a manifest mapping cells to progress/time. Frozen frames are jank-free, so it beats real-time capture for fast animations. CSS-driven animations only (JS/rAF animations are not seekable — errors clearly and suggests `sample`).
  - **`sample`** — real-time `requestAnimationFrame` motion recorder, decoupled from the blocking eval so event-triggered sweeps are catchable: `record=true` arms a watcher, trigger the animation, then `record=false` reads the measured per-frame curve, jank stats (dropped frames, max frame gap), and declared-vs-measured duration. Works for any animation including JS/rAF-driven ones.
- **`filmstrip` module** — composes raw RGBA frames into one grid PNG (pure Rust). `screenshot.rs` refactored to expose `capture_window_raw` (raw RGBA + dims) on Windows/macOS/Linux-X11; Wayland (grim, PNG-only) returns a clear error for raw capture.
- **`VictauriClient` methods** `animation_list`, `animation_scrub`, `animation_sample_arm`/`_read`.
- **demo-app** ships a deliberately-miscalibrated, re-triggerable slide-in (`#sweep-toast`/`#sweep-btn`) as a calibration target; agent-eval corpus gains task **T7** (calibrate the sweep).
- Verified live against the demo-app: `list` read the broken config exactly; `scrub` returned the overshoot curve (tx 420→473→…→−48) + a 2716×1212 filmstrip; `sample` recorded 145 frames over 1199.8ms with 0 jank.

### Added — Per-window introspectability diagnostic

- **`window introspectability`** action — probes every window's JS bridge and reports which ones Victauri can actually see vs. which are **blind**. A window that returns `introspectable:false` while `visible:true` is almost always missing the `victauri:default` capability: Tauri's per-window permission ACL silently blocks the bridge's callback IPC, so `eval_js`/`dom_snapshot`/`animation`/`find_elements` see nothing with no error. The diagnostic turns that silent dead-end into an actionable, up-front message naming the exact capability file to edit. Required per window (not just `main`) — the common multi-window gotcha. Verified live: flags a capability-stripped window correctly and passes a fully-capable one.

## [0.7.1] - 2026-05-31

### Changed

- **Resilience: `eval_js` fails fast when the webview reloads or the app stops responding.** Previously, if the bridge went away mid-session (e.g. an SPA navigation, a Tauri app's startup-recovery re-navigation, or an app crash), every subsequent eval blocked the full timeout (up to 30s) and returned an unclear result. Now, when an eval times out, the next eval on that window does a fast liveness probe (~2s) and fails immediately with a clear "the webview may have reloaded or the app stopped responding" message if the bridge is gone. Harmless when the bridge is alive — the re-probe succeeds in milliseconds (verified: a normal eval 70ms after a 30s timeout). The timeout message also now names the reload/crash case as a possible cause.

### Internal / docs

- Property tests for the `eval_js` auto-return heuristic (fuzz + multi-statement-corruption invariants) — the code that caused the worst bug, now locked down.
- New adversarial multi-step real-app E2E suite run against the demo-app in CI (`e2e-real-app` job) — the guard the original happy-path tests lacked.
- `bump-version` scripts now update the `[workspace.dependencies]` inter-crate pins automatically.
- Tools reference docs: added `route`/`trace`/trusted-input/iframe; corrected stale `query_db`/`wait_for`/`list_app_dir`/`read_app_file` params.

## [0.7.0] - 2026-05-30

### Added — Webview parity (Playwright-grade, no CDP)

- **Trusted (OS-level) input (Phase 2).** `input` (`type_text`/`press_key`) and `interact` (`click`) accept `trusted: true` to deliver real OS input events (`isTrusted: true`) instead of synthetic DOM events — for app handlers that gate on `event.isTrusted` and browser features needing user activation. Implemented on Windows via Win32 `SendInput` (Unicode keystrokes, named keys, and DPI-aware absolute mouse clicks); macOS/Linux return a clear "not implemented on this platform" error and callers fall back to synthetic input. New `WebviewBridge` methods `native_type_text`/`native_key`/`native_click`. Verified live on Windows: keyboard `keydown.isTrusted === true`, click `isTrusted === true` at the correct coordinates. (Cookie *set* for non-httpOnly cookies is available today via `eval_js` `document.cookie=...`; httpOnly cookie-set via the platform cookie store is deferred.)
- **`trace` tool — screencast / visual timeline (Phase 4).** `start` captures the window at a fixed interval (`interval_ms`, `max_frames` ring buffer) via the platform-native screenshot path; `with_events=true` also drives the event recorder. `stop` returns a summary (frame count, duration, timestamps, recorded event count); `frames` returns the captured PNGs; `status` reports progress. Pairs with `recording` and `logs` to form a Playwright-trace-style bundle — cross-platform, no CDP.
- **Same-origin iframe traversal (Phase 3).** `dom_snapshot` (compact + JSON) and `find_elements` now descend into same-origin `<iframe>`/`<frame>` content; cross-origin frames are marked and skipped. Elements inside frames get ref handles and are fully interactable (`interact`, `input`). Actionability checks are now frame-aware — the occlusion/viewport checks run against the element's own document/window, fixing false "covered by …" rejections for frame elements (`getBoundingClientRect` is relative to the element's own frame viewport).
- **`route` tool — network interception (Phase 1).** Match webview `fetch`/XHR by URL (`substring`/`glob`/`regex`/`exact`, optional method) and **block** (abort), **fulfill** (return a synthetic `status`/`headers`/`body`/`content_type` mock — fetch only), or **delay** (latency injection). `times` limits firings; `route matches` logs intercepts; rules are page-scoped. The Playwright `route()` equivalent, implemented purely in the JS bridge — works identically on WebView2/WKWebView/WebKitGTK. (XHR supports block/delay; fulfill is fetch-only. Top-level navigation, sub-resources, and WebSocket frames are not intercepted; for Tauri IPC-layer faults use `fault`.)

## [0.6.0] - 2026-05-30

### Fixed

- **CRITICAL: `eval_js` silently returned wrong values for multi-statement code.** The auto-return heuristic prepended `return` to any code not starting with a statement keyword, so a statement block like `foo(); return bar()` was rewritten to `return foo(); return bar()` — executing only the first statement and silently discarding the rest (typically returning `undefined`). This affected the extremely common "do X, then return Y" pattern (`localStorage.setItem(...); return localStorage.getItem(...)`, `window.scrollTo(...); return window.scrollY`, etc.). The heuristic is now string/comment/template-aware and only prepends `return` to a single bare expression; multi-statement code and code with an explicit `return` are used as-is.
- **Deeply-nested `eval_js` results leaked the internal envelope.** serde_json's default recursion limit (128) caused results nested deeper than ~127 levels to fail parsing and silently fall through to returning the raw `{"__victauri_ok":...}` envelope as a string. When the recursion-limited parse fails, the envelope is now stripped by string slicing (no recursion) so the actual value is returned. (The recursion limit is intentionally *not* disabled — an unbounded recursive parse/serialize of a pathologically deep result overflows the worker thread stack and crashes the host.)
- **`logs ipc`/`logs network`/`logs slow_ipc` and `detect_ghost_commands` failed on real apps.** These tools fetched the entire IPC/network log — including full request/response bodies — and exceeded the 5 MB eval cap on apps with substantial traffic (e.g. responses containing large arrays). They now apply a default entry limit (100) and truncate per-entry fields larger than 4 KB; `detect_ghost_commands` projects only command names; `slow_ipc` truncates each returned entry.
- **`window get_state` on a nonexistent label** now returns an error instead of an empty array (which read as "success, no state").
- **`window resize` with zero width/height** is now rejected with a clear error.
- **`eval_js` timeout message** now explains that JavaScript syntax errors surface only as a timeout (the webview cannot report parse errors back to the host), alongside unresolved promises and infinite loops.

### Added

- **`VictauriBuilder::db_search_paths(paths)`**: register extra directories for `query_db` and `introspect db_health` to search for SQLite databases, beyond the OS app directories. Many apps store their database in a project/working directory or a custom location that the default app-data search cannot reach. Configured roots take precedence in auto-discovery, and absolute `query_db` paths are permitted when they resolve within an allowed root (read-only and path-traversal-guarded as before).

### Security

- **`query_db` blocks the write form of PRAGMA** (`PRAGMA name = value`) explicitly. The connection was already opened `SQLITE_OPEN_READ_ONLY` (so writes could not persist), but the write form is now rejected up front so the read-only contract does not rely solely on the open flags. Read forms (`PRAGMA name`, `PRAGMA name(arg)`) remain allowed.

## [0.5.6] - 2026-05-28

### Changed

- **BREAKING: Auth enabled by default.** The MCP server now auto-generates a UUID v4 Bearer token on startup and writes it to the discovery directory (`<temp>/victauri/<pid>/token`). Clients using `VictauriClient::discover()` pick it up automatically — zero config change needed. To opt out: call `auth_disabled()` on `VictauriBuilder`. See Migration Guide for details.
- **Environment variable allowlist trimmed.** `get_diagnostics` now exposes 16 safe prefixes (down from ~30). Removed: `PATH`, `RUST*`, `CARGO*`, `APPDATA`, `LOCALAPPDATA`, `USERPROFILE`, `TEMP`, `TMP`, `PROGRAMFILES*`, `SYSTEMROOT`, `WINDIR`, `COMSPEC`, `PROCESSOR_*`, `NUMBER_OF_PROCESSORS`, `COMPUTERNAME`, `OLDPWD`.
- **Rate limiter 429 responses** now include `Retry-After: 1` header per RFC 6585.

### Added

- **DNS rebinding guard** (both plugin and browser crates): Middleware validates `Host` header is `localhost`, `127.0.0.1`, `[::1]`, or `localhost:<port>` — blocks DNS rebinding attacks via crafted hostnames.
- **Origin guard** (browser crate): URL-parsed origin validation blocks subdomain smuggling (e.g. `localhost.evil.com`). Rejects non-localhost origins and null origins.
- **Security response headers**: All responses include `X-Content-Type-Options: nosniff`, `Cache-Control: no-store`, `X-Frame-Options: DENY`, `Access-Control-Allow-Origin: null`, `Content-Security-Policy: default-src 'none'`.
- **SQL comment stripping**: `query_db` strips `--` line comments and `/* */` block comments before the read-only check, preventing comment-based injection bypasses.
- **Stacked query blocking**: `query_db` rejects queries containing `;` (multiple statements), preventing `SELECT 1; DROP TABLE` attacks.
- **Discovery file ACLs** (Windows): Port and token files in the discovery directory are restricted to the current user via `icacls /inheritance:r /grant:r <user>:F`.
- **Eval output size limit**: `eval_js` results capped at 5 MB (`MAX_EVAL_RESULT_LEN`). Oversized results return an error with the actual size, preventing memory exhaustion from `JSON.stringify` on large DOM trees.

### Fixed

- Browser crate `rate_limit()` now returns proper 429 with `Retry-After` header (was returning bare 429).
- Rate limiter concurrent test assertion widened to account for token refill timing variance.

## [0.5.5] - 2026-05-28

### Added

- **`AppEvent::Console` variant:** Console log events now have a dedicated event type instead of being mapped to `StateChange` — cleaner typing for explain narratives and recording
- **`AppEvent::is_internal()`:** Centralised check for Victauri infrastructure events (replaces scattered string-matching)
- **Bridge ready signal:** JS bridge sends `__victauri_bridge_ready__` callback on initialization — eval pipeline waits for this signal instead of racing on first eval
- **Discovery session tokens:** Server always writes a session token to the discovery directory — clients auto-read it for future zero-config auth
- **Cross-platform E2E CI:** Demo app E2E tests now run on Linux (xvfb), macOS, and Windows in CI
- **Regression E2E tests:** 8 targeted tests validating all v0.5.3/v0.5.4 fixes (eval errors, IPC log purity, recording after stop, explain noise, CSS selector errors, checkpoint labels)
- **Soak test:** `soak_test.rs` — 120-second longevity test checking memory growth, latency degradation (`VICTAURI_SOAK=1`)
- **Concurrent stress test:** `concurrent_stress_test.rs` — 10-client concurrent tool exercise for 60 seconds (`VICTAURI_STRESS=1`)
- **IPC capture health check:** `check_ipc_integrity` warns when zero IPC entries but >5 network requests detected

## [0.5.4] - 2026-05-27

### Fixed

- **Eval envelope protocol:** Replaced `__error` key convention with `__victauri_ok`/`__victauri_err`/`__victauri_type` envelope — eliminates false positives when user JS returns objects with `__error` key, and distinguishes `undefined` value from `"undefined"` string
- **XHR interceptor:** Added `isVictauriInternal` filter to XMLHttpRequest interceptor — Victauri IPC no longer leaks into network log via XHR path
- **Explain narrative:** Filters now check `key.starts_with("console.")` instead of `caused_by.contains("victauri")` — app console logs mentioning "victauri" are no longer suppressed
- **Recording methods:** `events_since()`, `events_between()`, `get_checkpoints()`, and `events_between_checkpoints()` now fall back to `last_session` after `stop()` — agents can query recording data after stopping
- **Bridge probe caching:** Probes are cached per window label, preventing redundant 2-second probes on repeated calls to the same window
- **WeakRef map cleanup:** Full `weakRefMap` sweep on every `snapshot()` call — GC'd element entries are removed, preventing map growth in long sessions
- **Drain loop injection safety:** UUID interpolation in drain loop now uses `js_string()` helper instead of raw string interpolation

### Added

- **Recording flush:** New `recording.flush` action triggers immediate one-shot event drain instead of waiting for the 1-second polling interval
- **query_db expanded search:** Database discovery now searches `app_data_dir`, `app_config_dir`, `app_local_data_dir`, and `app_log_dir` (deduplicated)

## [0.5.3] - 2026-05-27

### Fixed

- **Release Blocker:** Victauri's own IPC traffic (`plugin:victauri|*`) no longer fills the 1000-entry `networkLog` — real app IPC evidence is preserved
- **Release Blocker:** Multi-window eval (hidden windows) now fails fast with diagnostic in 2s instead of timing out after 30s — bridge probe detects unresponsive windows
- **Release Blocker:** `eval_js` errors surface as MCP `isError` — `throw new Error()` returns structured error, `undefined` returns `"undefined"`, `null` returns `"null"`
- **Recording:** `replay` and `export` now work after `stop()` — session data persisted in `last_session` field
- **Recording:** `checkpoint_label` parameter now accepts `label` as alias via `#[serde(alias)]`
- **find_elements:** Invalid CSS selectors now return descriptive error instead of silently returning `[]`
- **explain:** Narrative (summary/last_action/diff) no longer dominated by Victauri's own drain loop callbacks — internal IPC and state changes filtered out

## [0.5.2] - 2026-05-26

### Changed

- **BREAKING (introspect tool):** `managed_state` action renamed to `plugin_state` for clarity
- **BREAKING (introspect tool):** `tasks` action renamed to `plugin_tasks` to distinguish from app tasks
- **victauri-plugin**: `introspect.capabilities` now returns structured security config (CSP, `freeze_prototype`), configured plugins, window definitions, and privacy profile — previously returned only basic config
- **victauri-plugin**: `introspect.processes` now enumerates child processes (sidecars, background workers) with PID, name, and memory usage — previously returned only the host process info
- **victauri-plugin**: `introspect.event_bus` events are now captured automatically via `listen_any` — apps no longer need to manually push events

### Added

- **victauri-plugin**: `VictauriBuilder::listen_events(&["event-name", ...])` — register custom Tauri event names to capture in the event bus (window lifecycle events are captured automatically)
- **victauri-plugin**: Automatic window lifecycle event capture — resize, move, focus, close, theme change, drag-drop events are pushed to `EventBusMonitor` without app opt-in
- **victauri-plugin**: `enumerate_child_processes()` with platform-native APIs: Windows `CreateToolhelp32Snapshot`, Linux `/proc`, macOS `proc_listchildpids`
- **victauri-plugin**: `tauri_config()` now exposes window definitions, plugin list, and security configuration (capabilities, CSP)

### Removed

- **victauri-plugin**: `introspect.fs_scope` action removed (redundant with `app_info` tool which already provides directory paths)

### Fixed

- Chrome/Firefox extension popup version display updated from v0.1.0 to v0.5.0
- VS Code extension `package-lock.json` version synced to 0.5.0
- Social preview SVG tool count updated from 28 to 31
- VS Code `esbuild` bumped to ^0.25.0 (resolves moderate security advisory GHSA-67mh-4wv8-2f99)
- npm audit now reports 0 vulnerabilities across all JS packages

## [0.5.0] - 2026-05-26

### Added

- **victauri-plugin**: `introspect` compound tool with 15 actions for deep backend introspection — `command_timings` (per-command min/max/avg/p95), `coverage` (session command usage), `contract_record`/`contract_check`/`contract_list`/`contract_clear` (IPC schema drift detection), `startup_timing` (plugin init phases), `capabilities` (Tauri v2 permission audit), `db_health` (SQLite diagnostics), `managed_state` (full plugin internals), `processes` (PID, platform, arch), `tasks` (tracked async task status), `fs_scope` (app directory paths), `event_bus`/`event_bus_clear` (combined Tauri + app event timeline)
- **victauri-plugin**: `fault` compound tool for IPC chaos engineering — `inject` (delay/error/drop/corrupt fault types with optional trigger limits), `list`, `clear`, `clear_all`. CDP cannot inject failures at the backend IPC layer.
- **victauri-plugin**: `explain` compound tool for natural-language event narration — `summary` (aggregate events into narrative with type counts), `last_action` (causal chain with arrows), `diff` (count IPC/DOM/errors/interactions over time window)
- **victauri-plugin**: `recording.replay` action — re-executes all IPC commands from a recorded session, compares response shapes, reports per-command pass/fail with shape diff on drift
- **victauri-plugin**: `EventBusMonitor` — `Arc<RwLock<VecDeque<CapturedTauriEvent>>>` ring buffer (1000 capacity) for Tauri native events, combined with `EventLog` in `introspect.event_bus`
- **victauri-plugin**: `TaskTracker` — tracks spawned async tasks (MCP server, event drain loop, on_ready probe) via `Arc<AtomicBool>` finished flags
- **victauri-plugin**: Managed state introspection via `introspect.managed_state` — serializes full `VictauriState` internals: event counts, registry size, recording state, active faults, contract baselines, timing data, task status, tool invocations, uptime, port
- **victauri-plugin**: `FaultRegistry` (thread-safe `RwLock<HashMap>`) with `CommandTimings` (per-command timing stats), `ContractStore` (IPC contract baselines with JSON shape diffing), `StartupTimeline` (plugin init phase timestamps)
- **victauri-plugin**: `JsonShape` recursive type structure extraction from JSON for contract comparison; `diff_shapes()` detects new/removed fields and type changes
- **victauri-core**: `EventLog.since(timestamp)` for time-windowed queries with `chrono::TimeDelta`

### Changed

- Tool count increased from 30 to 31 (19 standalone + 12 compound)
- Bridge version bumped to 0.5.0

## [0.4.0] - 2026-05-26

### Changed

- **BREAKING:** **victauri-plugin**: Authentication **disabled by default** — the MCP server binds to `127.0.0.1` only and the plugin is `#[cfg(debug_assertions)]`-gated, so auth adds friction without meaningful security for local dev. Use `auth_enabled()`, `auth_token("...")`, or `VICTAURI_AUTH_TOKEN` env var to opt in.
- **victauri-plugin**: `auth_disabled()` is now a backwards-compatible no-op (auth is already off by default)
- **victauri-plugin**: `generate_auth_token()` now delegates to `auth_enabled()` logic

### Added

- **victauri-plugin**: `VictauriBuilder::auth_enabled()` — opt-in auth with auto-generated UUID token
- **victauri-plugin**: `VictauriBuilder::register_command_names(&["cmd1", "cmd2"])` — lightweight command registration without proc macros
- **victauri-plugin**: `VictauriBuilder::commands(&[CommandInfo])` — register full command schemas
- **victauri-cli**: `victauri invoke <command> [--args '{}']` — call any Tauri IPC command from terminal
- **victauri-cli**: `victauri doctor` — full setup diagnosis
- **victauri-cli**: `victauri init` now scaffolds CLAUDE.md with agent instructions that make AI agents prefer Victauri over CDP/Playwright
- **ci**: Production-ready GitHub Action at `.github/actions/victauri-test/` with branding, diagnostics, and coverage support

## [0.3.0] - 2026-05-24

### Changed

- **victauri-plugin**: IPC `wait_for_capture` replaced polling loop (50ms×10) with event-driven signaling — fetch interceptor now notifies waiters after response body parsing completes, eliminating 500ms worst-case latency
- **victauri-test**: `VisualOptions` defaults to `platform_baselines: true` — baselines stored in `tests/snapshots/{os}/` for cross-platform CI

### Added

- **victauri-test**: `MaskRegion` — exclude rectangular areas from visual comparison (timestamps, animations, user-specific content)
- **victauri-test**: `ThresholdPreset` enum — `Strict` (pixel-perfect), `Standard` (default), `AntiAlias` (subpixel-tolerant), `Relaxed` (cross-platform lenient)
- **victauri-test**: `VisualOptions::with_preset()` and `with_mask()` fluent builders
- **victauri-test**: `VisualDiff.masked_pixels` field reports excluded pixel count
- **victauri-test**: `VictauriClient::dom_snapshot_for(label)` — DOM snapshot targeting a specific webview
- **victauri-test**: `VictauriClient::screenshot_for(label)` — screenshot of a specific window by label
- **victauri-test**: `VictauriClient::is_alive()` — quick health check without session overhead
- **victauri-test**: `VictauriClient::reconnect(max_wait)` — re-establish MCP session after app restart, polls health with 250ms backoff
- **victauri-test**: `VictauriClient::get_ipc_calls_for(command)` — replaces `get_ipc_calls` with clearer preposition
- **victauri-test**: `VictauriClient::get_ipc_calls_since(checkpoint)` — replaces `ipc_calls_since` with verb-first naming

### Deprecated

- **victauri-test**: `VictauriClient::get_ipc_calls()` — use `get_ipc_calls_for()` instead
- **victauri-test**: `VictauriClient::ipc_calls_since()` — use `get_ipc_calls_since()` instead
- **victauri-test**: `VictauriClient::ipc_checkpoint()` — use `create_ipc_checkpoint()` instead

## [0.2.0] - 2026-05-10

### Security

- **victauri-plugin**: Origin guard rewritten with URL parsing — `starts_with("http://localhost")` replaced with `url::Url::parse()` + host comparison, blocking `localhost.evil.com` and `localhost@evil.com` prefix smuggling attacks
- **victauri-plugin**: Action-level privacy in strict mode — `invoke_command`, `window.manage`, `window.resize`, `window.move_to`, `window.set_title` now blocked alongside existing tool-level gates
- **victauri-plugin**: `file:` URL navigation blocked by default — `VictauriBuilder::allow_file_navigation()` to opt in
- **victauri-plugin**: `RegexSet::new().expect()` replaced with match + tracing fallback
- **victauri-plugin**: `deflate_compress` returns `Result` instead of panicking

### Changed

- **BREAKING:** **victauri-plugin**: `PrivacyProfile` enum replaces boolean `strict_privacy_mode()` — three tiers: `Observe` (read-only), `Test` (observe + interactions + input + recording), `FullControl` (everything, default). `strict_privacy_mode()` now maps to `Observe` profile. New `privacy_profile(PrivacyProfile)` builder method.
- **victauri-plugin**: `interact` tool now gated by privacy profile — blocked in `Observe`, allowed in `Test` and `FullControl`
- **victauri-plugin**: `recording` tool now gated by privacy profile — blocked in `Observe`, allowed in `Test` and `FullControl`
- **victauri-plugin**: `get_plugin_info` now reports `privacy.profile` field (`"observe"`, `"test"`, `"full_control"`)
- **victauri-plugin**: `invoke_command` in `Test` profile requires command to be on the allowlist

### Added

- **victauri-core**: `acquire_lock`, `acquire_read`, `acquire_write` helpers for mutex/rwlock poisoning recovery with tracing diagnostics (replaces 28 raw `PoisonError::into_inner` calls)
- **victauri-core**: `DomElement.attributes` and `DomSnapshot.ref_map` changed from `HashMap` to `BTreeMap` for deterministic serialization
- **victauri-test**: Per-process server discovery directories (`<temp>/victauri/<pid>/`) for CI parallelism with TCP-based liveness filtering
- **victauri-test**: `TestApp` stderr capture — connection timeout errors now include last 10 lines of app stderr
- **victauri-test**: 12 new `VictauriClient` methods: `double_click`, `hover`, `click_by_selector`, `fill_by_text`, `fill_by_selector`, `select_option_by_id`, `select_option_by_text`, `select_option_by_selector`, `scroll_to_by_id`, `scroll_to_by_selector`, `double_click_by_id`, `double_click_by_text`
- **victauri-test**: Codegen compile test harness validates all generated method names exist on `VictauriClient`
- **victauri-plugin**: Centralized output redaction at `call_tool` boundary — applies to all text responses uniformly
- **victauri-plugin**: Per-process metadata.json written alongside port/token files (PID, port, version, timestamp)
- **victauri-cli**: `--allow-empty-registry` flag for `coverage` command; exits 1 on empty registry by default
- CI coverage job with `cargo-llvm-cov` and Codecov upload
- Release workflow: dry-run + test gate before publish, `victauri-cli` added to publish sequence
- `MIGRATION.md` — upgrade guide for v0.1.x → v0.2.0
- **victauri-test**: Visual regression testing — `compare_screenshot()` with pixel-level PNG diffing, configurable channel tolerance, diff image generation, RGB/Grayscale auto-conversion
- **victauri-test**: `VictauriClient::screenshot_visual()` convenience method — capture + compare in one call
- **victauri-test**: IPC coverage tracking — `coverage_report()` compares registered commands against observed calls, `assert_coverage_above()` for threshold enforcement
- **victauri-test**: `VerifyBuilder::coverage_above()` for fluent coverage assertions
- **victauri-test**: `JunitReport` — generate JUnit XML reports from `VerifyReport` for CI integration
- **victauri-core**: Test codegen engine — `generate_test()` converts `RecordedSession` into compilable Rust test code with idiomatic selector resolution (`click_by_id`, `click_by_text`, raw fallback)
- **victauri-core**: `DomInteraction` event type with `InteractionKind` enum (Click, DoubleClick, Fill, KeyPress, Select, Navigate, Scroll)
- **victauri-core**: `inventory`-based command auto-discovery via `CommandInfoFactory`
- **victauri-plugin**: JS interaction observer — captures click, dblclick, change, keydown with `isTrusted` check and `bestSelector()` resolution
- **victauri-plugin**: `parse_bridge_event()` public API for unit-testable event parsing
- **victauri-cli**: `coverage` command — report IPC coverage with optional `--threshold` and `--junit` flags
- **victauri-cli**: `record` command — connect to running app, capture interactions, generate test file
- **victauri-cli**: `watch` command — re-run tests automatically on file changes via `notify` crate
- **victauri-cli**: `init` command — scaffold test directory with starter smoke tests
- **victauri-test**: `WaitForBuilder` fluent API — `client.wait("text").value("Hello").timeout_ms(15_000).run().await` as alternative to positional `wait_for()`
- **victauri-test**: `PluginInfo` and `MemoryStats` typed response structs — `plugin_info()` and `memory_stats()` methods with deserialized returns alongside raw JSON `get_plugin_info()`/`get_memory_stats()`
- **victauri-test**: `create_ipc_checkpoint()` verb-first canonical name — `ipc_checkpoint()` deprecated with forwarding alias
- **victauri-test**: `TestError::Connection` now carries structured `host`, `port`, `reason` fields instead of a flat string
- **victauri-test**: `VictauriClient` exposes `host()` and `port()` accessors
- **victauri-plugin**: `logs` tool `wait_for_capture` parameter — polls up to 500ms for pending IPC responses before returning log, eliminating race conditions in test assertions

## [0.1.2] - 2026-05-07

### Fixed

- **victauri-plugin (macOS)**: `extern "C"` block changed to `unsafe extern "C"` for Rust 2024 edition compatibility -- previously failed to compile on macOS CI runners with `error: extern blocks must be unsafe`
- **victauri-plugin (macOS)**: Added process memory stats via `task_info(MACH_TASK_BASIC_INFO)` -- previously returned "memory stats not available on this platform"
- **victauri-plugin**: Rate limiter integration test rewritten to use injectable `RateLimiterState` via new `build_app_full()` -- previously flaky because sequential requests couldn't outpace the 1000-token/sec default refill
- **victauri-plugin README**: Corrected tool count from "55 tools / 17 categories" to accurate "23 tools (9 compound + 14 standalone)" with full tool table
- Multiple clippy lint fixes for cross-platform CI: `cast_lossless`, `items_after_statements`, `doc_markdown`, `map_unwrap_or`, nul-terminated C-string literals, redundant pointer casts

### Changed

- **CI**: Added `fail-fast: false` to check and test matrix jobs so one platform failure no longer cancels the others
- **CI**: Added `npm install` step for jsdom bridge tests with `working-directory` (Windows compatible)
- **victauri-plugin**: Bridge tests gracefully skip when jsdom is not installed

### Added

- `build_app_full()` public API for constructing axum router with custom rate limiter (useful for testing)
- macOS process memory reporting (`virtual_bytes`, `resident_bytes`, `resident_max_bytes`)
- CODE_OF_CONDUCT.md (Contributor Covenant v2.1)

## [0.1.1] - 2026-05-01

### Fixed

- **victauri-test**: `VictauriClient` now correctly parses SSE (`text/event-stream`) responses from rmcp MCP servers — previously `call_tool()` would fail with JSON parse errors on valid responses
- **victauri-plugin (Windows)**: Screenshot now captures WebView2 content by using `PW_RENDERFULLCONTENT` flag with `PrintWindow` — previously returned blank images because WebView2 uses GPU/DirectComposition rendering that `PW_CLIENTONLY` alone cannot capture

### Added

- **victauri-test**: 75 E2E integration tests against the demo app covering all 23 tools, 3 resources, authentication, concurrent sessions, cross-boundary verification, and edge cases
- **victauri-test**: Auto-discovery of port and auth token via temp files (`victauri.port`, `victauri.token`) with env var and default fallbacks
- **victauri-plugin**: Port fallback — tries ports 7374-7383 if preferred port is taken, writes actual port to temp file for client discovery
- **victauri-plugin**: Auto-event recording background loop — polls `getEventStream()` every 1s during recording, eliminating manual event capture
- **victauri-plugin**: Rate limiter bumped to 1000 req/sec default for test workloads
- **victauri-test**: `connect_with_token()` for authenticated connections

### Changed

- **BREAKING:** `SemanticAssertion.condition` is now `AssertionCondition` enum instead of `String` — invalid conditions are caught at deserialization, not deep in evaluation logic
- **BREAKING:** All 9 compound tool `action` parameters are now typed enums (`InteractAction`, `InputAction`, `WindowAction`, `StorageAction`, `NavigateAction`, `RecordingAction`, `InspectAction`, `CssAction`, `LogsAction`) — invalid actions are rejected at JSON deserialization with clear variant listings
- **BREAKING:** `WaitForParams.condition` is now `WaitCondition` enum instead of `String`
- **BREAKING:** `StorageParams.storage_type` is now `Option<StorageType>` enum, `NavigateParams.dialog_type`/`dialog_action` are now `Option<DialogType>`/`Option<DialogAction>` enums, `WindowParams.manage_action` is now `Option<ManageAction>` enum
- **BREAKING:** `SnapshotParams.format` is now `Option<SnapshotFormat>` enum instead of `Option<String>`
- `events_between_checkpoints` returns `Result` with specific error variants instead of `Option`
- Extracted `json_result` helper in MCP handler, eliminating 14 repeated serialization blocks
- All match-based extractions replaced with `let...else` (32 sites)
- All `map().unwrap_or()` chains replaced with `map_or()` (10 sites)
- All redundant closures replaced with method references (53 sites)
- `Default::default()` replaced with explicit type calls per `default_trait_access`
- Scoped `use` imports moved before statements per `items_after_statements`

### Added (code quality)

- `AssertionCondition` enum with `FromStr`, `Display`, `Serialize`/`Deserialize`, and feature-gated `JsonSchema` (`schema` feature on victauri-core)
- 16 typed enums replacing string parameters across plugin and core crates
- `Serialize` and `Display` implemented on all 13 action enums for symmetric serde and ergonomic formatting
- `#[must_use]` on all 26 value-returning public functions (constructors, getters, builders, analysis)
- `# Errors` documentation on all public `Result`-returning functions
- `# Panics` documentation on all functions containing panicking assertions
- Backticks on all code items in doc comments (73 sites)
- Crate-level documentation on all crates, binaries, and build scripts
- `#![deny(missing_docs)]` enforced in victauri-core, victauri-plugin, and victauri-test — missing doc comments fail the build
- `FLOAT_EPSILON` named constant for floating-point severity classification
- **20 clippy lints enforced at deny level** (17 pedantic + 3 nursery) in workspace config: `redundant_closure_for_method_calls`, `missing_errors_doc`, `must_use_candidate`, `return_self_not_must_use`, `manual_let_else`, `map_unwrap_or`, `doc_markdown`, `uninlined_format_args`, `single_match_else`, `default_trait_access`, `cast_lossless`, `needless_raw_string_hashes`, `if_not_else`, `missing_panics_doc`, `items_after_statements`, `clippy::all`, `derive_partial_eq_without_eq`, `use_self`, `redundant_pub_crate`
- Centralized lint configuration in `[workspace.lints]` (Cargo edition 2024)
- `CommandInfo::new()` builder pattern with `with_description`/`with_intent`/`with_category` — eliminates 9-field struct literal boilerplate
- `Display` implemented on 10 public types: `VerificationResult`, `Divergence`, `DivergenceSeverity`, `GhostCommandReport`, `GhostCommand`, `GhostSource`, `IpcIntegrityReport`, `IpcCall`, `IpcResult`, `ScoredCommand`
- `From<IpcCall> for AppEvent` conversion
- `Divergence` and `DivergenceSeverity` re-exported from crate root
- 26 runnable doc-test examples across core + test crates (up from 6): `verify_state`, `detect_ghost_commands`, `check_ipc_integrity`, `EventLog::push`, `EventLog::ipc_calls`, `EventRecorder::start`/`stop`, `VerificationResult` (construction + Display), `CommandRegistry::search`/`resolve`, `CommandInfo::new`, `DomSnapshot::to_accessible_text`, `GhostCommandReport::Display`, `IpcIntegrityReport::Display`, `assert_json_eq`, `assert_json_truthy`, `assert_no_a11y_violations`, `assert_performance_budget`, `assert_ipc_healthy`, `assert_state_matches`
- Named constants replacing magic numbers: PNG encoding (`PNG_SIGNATURE`, `CRC32_POLYNOMIAL`, `ADLER32_MOD`), server (`DEFAULT_WEBVIEW_LABEL`), auth (`BEARER_PREFIX_LEN`), builder validation (`MAX_EVENT_CAPACITY`, `MAX_RECORDER_CAPACITY`, `MAX_EVAL_TIMEOUT_SECS`), recorder (`DEFAULT_MAX_EVENTS`)
- `// SAFETY:` comments on all `unsafe` blocks (FFI, macOS bridge, watchdog env tests)
- `Eq, PartialEq` derived on 18 core data types for ergonomic `assert_eq!` in tests
- `clippy.toml` with `too-many-lines-threshold = 100` and `type-complexity-threshold = 300`

### Removed

- 39 dead param structs from pre-compound-tool era (replaced by compound params)
- `tool_not_found` helper (no longer needed with typed action enums)
- `introspection_params.rs` and `recording_params.rs` modules (all structs superseded)
- Dead `RecoveryHint` variants (`RetryLater`, `TryAlternative`) and `ref_not_found` helper
- Per-crate `#![deny(unsafe_code)]` attributes (now in workspace lints)

### Fixed (code quality)

- `score_command` per-word score normalization — multi-word queries no longer inflate scores ~Nx compared to single-word queries, making cross-query ranking reliable
- 6 ghost tool names in `VictauriClient` test client (`fill`, `type_text`, `get_window_state`, `get_ipc_log`, `start_recording`, `wait_for` parameter name)
- Benchmark code silently discarding `Result` from `EventRecorder::start`
- README code examples using non-existent API methods

## [0.1.0] - 2026-04-28

Initial public release.

### Added

- Workspace with 5 crates: victauri-core, victauri-macros, victauri-plugin, victauri-test, victauri-watchdog
- 23 MCP tools (9 compound + 14 standalone) covering WebView, Windows, Backend, Storage, Navigation, Verification, Time-Travel, CSS/Style, Accessibility, Performance
- 3 MCP resources: victauri://ipc-log, victauri://windows, victauri://state
- `#[inspectable]` proc macro for Tauri command instrumentation with JSON schema generation
- JS bridge v0.3.0 with DOM walking, ref handles, console capture, mutation observer, network interception, navigation tracking, dialog capture, waitFor polling
- IPC interception via `fetch` monkey-patching (Tauri 2.0 `ipc.localhost` protocol)
- Privacy layer: command allowlists/blocklists, tool disabling, regex-based output redaction, strict mode
- Rate limiting (1000 req/sec default, token bucket with AtomicU64)
- `VictauriBuilder` for port/auth/capacity configuration
- `VICTAURI_PORT` and `VICTAURI_AUTH_TOKEN` environment variable support
- Release-safe: zero overhead in release builds via `#[cfg(debug_assertions)]`
- Cross-platform CI (Linux, Windows, macOS) with clippy, tests, docs, MSRV, security audit, dependency checks, semver checks
- 415+ tests (121 core + 4 macro + 123 plugin unit + 118 integration + 38 adversarial + 5 watchdog + 6 doctests)
- 16 Criterion benchmarks across 5 groups
- Windows screenshot via `PrintWindow` + custom PNG encoder (no external dependencies)
- macOS screenshot via `CGWindowListCreateImage` + alpha un-premultiply
- Linux screenshot via X11 `GetImage` (x11rb) with Wayland fallback via `grim`
- OS-level process memory stats (Windows `GetProcessMemoryInfo`, macOS `task_info`, Linux `/proc/self/statm`)
- victauri-watchdog crash-recovery sidecar with configurable recovery commands
- victauri-test crate: typed MCP HTTP client (`VictauriClient`) with session management and assertion helpers (`assert_json_eq`, `assert_json_truthy`, `assert_no_a11y_violations`, `assert_performance_budget`, `assert_ipc_healthy`, `assert_state_matches`)
- Demo app example with 12 instrumented commands (greet, counter CRUD, todo CRUD, settings, state dump)
- Shadow DOM traversal in DOM snapshots
- Cross-boundary state verification, ghost command detection, IPC integrity checking
- Event recording with checkpoints, export/import, and replay sequences
- CSS inspection, visual debug overlays, accessibility auditing, performance profiling
- Tag-triggered release workflow for crates.io publishing

### Security

- JS injection prevention: all manual string escaping replaced with `serde_json::to_string()` via `js_string()` helper
- URL validation via scheme allowlist (http/https/file only) using the `url` crate parser
- DNS rebinding protection for localhost server
- Security headers (X-Frame-Options, X-Content-Type-Options, Cache-Control)
- Screenshot error handling: `GetDIBits()` return value checked on Windows

[Unreleased]: https://github.com/runyourempire/victauri/compare/v0.5.3...HEAD
[0.5.3]: https://github.com/runyourempire/victauri/compare/v0.5.2...v0.5.3
[0.5.2]: https://github.com/runyourempire/victauri/compare/v0.5.0...v0.5.2
[0.5.0]: https://github.com/runyourempire/victauri/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/runyourempire/victauri/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/runyourempire/victauri/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/runyourempire/victauri/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/runyourempire/victauri/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/runyourempire/victauri/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/runyourempire/victauri/releases/tag/v0.1.0
