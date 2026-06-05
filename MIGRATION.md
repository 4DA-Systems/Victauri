# Migration Guide

## v0.7.6 → v0.7.7 (bug fix — no action required)

Crates-only patch. `victauri test`'s smoke suite no longer fails on headless CI (the screenshot
check now tolerates the absence of a native window handle). No API or behavior changes. npm and
the VS Code extension are unchanged. If you pinned `victauri-test`/`victauri-cli` `=0.7.6` and run
`victauri test` in a headless environment, upgrade to `0.7.7`.

## v0.7.5 → v0.7.6 (async-completion + app-state probes — no breaking changes)

All changes are additive; no action required. To take advantage of the new features:

- **Await fire-and-forget backend work instead of sleeping.** `wait_for` has two new
  conditions. Use `expression` to poll any JS/status expression until truthy (it may
  `await`), and `event` to block until a named Tauri event fires. The robust pattern is
  `invoke_command(...)` then `wait_for(expression|event, ...)`. New client helpers:
  `wait_for_expression`, `wait_for_event`.
- **Expose app internals via `app_state` probes (opt-in).** Register probes on the
  builder — `VictauriBuilder::probe("scoring", || json!({ "pipeline_version": v, "stale": n }))` —
  then read them with the `app_state` tool (`client.app_state(Some("scoring"))`). The
  idiomatic pattern is to build your shared state as an `Arc` once and clone it into both
  Tauri's `.manage()` and the probe closure. To capture custom completion events for the
  `wait_for` `event` condition, add them to `listen_events(&["analysis-complete"])`.
- **Better connection failures.** `VictauriClient::discover` and `victauri doctor` now
  explain *why* a connection failed (app crashed/rebuilding vs. never started vs. release
  build). No code change needed — the error message is just more actionable.
- **`query_db` remains read-only by design.** To mutate state for a test, drive the app's
  own commands via `invoke_command` (which respects app invariants) rather than the DB.

## v0.7.4 → v0.7.5 (red-team hardening — no breaking changes)

All changes are additive or strictly more permissive; no action required. Two
behavioural notes:

- **`resolve_command` ranking improved.** A query that exactly matches a command's
  `intent` now receives a strong exact-match bonus, so it ranks above commands whose
  *name* merely contains one of the query words (e.g. `"increase counter"` now resolves
  to `increment`, not `get_counter`). If you asserted on exact `resolve_command`
  *ordering*, re-check those expectations — relevance is now more correct.
- **`assert_semantic` `label` and `recording.checkpoint` `checkpoint_id` are now
  optional.** Calls that previously failed with a missing-parameter error now succeed
  (`label` defaults to empty; an omitted `checkpoint_id` is auto-generated as
  `cp-<uuid>` and returned in the response). Existing callers that pass these are
  unaffected.

## v0.7.3 → v0.7.4 (agent connection reliability)

Recommended (not required): **connect agents through `victauri bridge`, not a fixed `url:`.**
A hardcoded `"url": "http://127.0.0.1:7373/mcp"` can point at the wrong app when several
Victauri apps run (or one falls back off a busy 7373), and it can't recover when the app
restarts. The bridge resolves the live backend by app identity and re-establishes the session
automatically. Update `.mcp.json` to:

```json
{ "mcpServers": { "victauri": { "command": "victauri",
  "args": ["bridge", "--wait", "--app", "<your.bundle.identifier>"] } } }
```

`--app` is only needed when several Victauri apps run at once (it pins the right one); with a
single app you can omit it. `victauri init` now writes this automatically (reading your
identifier from `tauri.conf.json`). Existing `url:` configs keep working — this is an
opt-in reliability upgrade. The `victauri bridge` command also gained a `--app` flag and now
re-discovers the port + re-initializes the session across app restarts.

## v0.7.2 → v0.7.3 (security / red-team hardening)

Mostly transparent. Two behaviour changes to be aware of:

- **`app_info.databases` shape changed.** It now returns an **array of objects**
  `{ path, size_bytes, webview_internal, selected }` (across configured `db_search_paths`
  and every OS app dir) instead of an array of `data_dir`-relative path strings. If you
  parsed the old string array, read `entry.path` instead. The new fields let an agent
  disambiguate the real application DB from WebView/engine internal stores.
- **`query_db` / `introspect db_health` selection is stricter.** WebView/browser-engine
  internal databases are now excluded from auto-selection, the largest real candidate
  wins, and configured `db_search_paths` are **exclusive** when set (no silent fallback to
  the OS app dirs). If you relied on the old "first file found" behaviour, pass an explicit
  `path`, or register the right directory via `VictauriBuilder::db_search_paths`. When only
  WebView internals are present, `query_db` now returns a clear error instead of querying
  the wrong DB.
- **`css inject` rejects remote `@import` / `url(...)` by default.** If you intentionally
  inject CSS that references a remote stylesheet/asset, pass `allow_remote: true`.
- **`eval_js` reports a likely syntax error in ~0.75s** instead of blocking for the full
  eval timeout. Valid-but-slow code (e.g. a `wait_for` poll) is unaffected.
- **Auth was already on by default** (since v0.5.6) — docs that still implied otherwise are
  corrected. No code change required.

## v0.7.1 → v0.7.2

Additive release — no breaking API changes, and it stays within the `^0.7`
range, so `victauri-* = "0.7"` consumers pick it up with no requirement change.
Existing code keeps working unchanged.

- **New `animation` MCP tool (34 tools total):** motion introspection via the Web
  Animations API (no CDP) — `list` (running animations + timing/easing/keyframes),
  `scrub` (deterministic pause-seek geometry curve + optional filmstrip PNG), and
  `sample` (real-time rAF motion + jank recorder). CSS-driven animations are
  seekable; JS/rAF-driven ones are observable via `sample` but not `scrub`.
- **New `window introspectability` action:** probes every window and reports which
  Victauri can actually see. If a multi-window app's secondary windows return
  `introspectable:false`, add `victauri:default` to that window's capability file
  (`src-tauri/capabilities/*.json`) — the bridge requires the capability **per
  window**, not just for `main`, and a rebuild is needed (capabilities are baked at
  compile time). This was always required; the diagnostic just makes it visible.
- **`screenshot::capture_window_raw`** is now public (raw RGBA + dims) alongside
  `capture_window` (PNG). Only relevant if you call the screenshot module directly.
- **New `filmstrip` module** (`compose`, `Frame`, `default_cols`) — public, additive.

## v0.6.0 → v0.7.0

Additive release — webview Playwright-parity (no CDP). No breaking API changes
for normal plugin usage.

- **Two new MCP tools:** `route` (network interception — block/fulfill/delay on
  fetch+XHR) and `trace` (screencast ring buffer + event bundle). 33 tools total.
- **Trusted OS input:** `input` (`type_text`/`press_key`) and `interact` (`click`)
  accept `trusted: true` for real OS events (`isTrusted: true`). Windows-implemented;
  macOS/Linux return a clear error and you keep using synthetic input (the default).
- **Same-origin iframe traversal:** `dom_snapshot`/`find_elements` now reach into
  frame content automatically — no API change, just broader coverage.
- **New `WebviewBridge` trait methods** (`native_type_text`/`native_key`/`native_click`)
  have default implementations, so existing `WebviewBridge` impls keep compiling.
- **`VictauriState` gained a public `screencast` field.** Only relevant if you
  construct `VictauriState` directly (tests/mocks) rather than via `VictauriBuilder`;
  add `screencast: std::sync::Arc::new(victauri_plugin::screencast::Screencast::default())`.

## v0.5.6 → v0.6.0

### Behavior change: `eval_js` multi-statement code

`eval_js` previously prepended `return` to any code that did not start with a
statement keyword. This silently broke multi-statement snippets — e.g.
`localStorage.setItem('k','v'); return localStorage.getItem('k')` was rewritten
to `return localStorage.setItem(...); ...` and returned `undefined`.

It now correctly only prepends `return` to a single bare expression. Multi-statement
code with an explicit `return` (or wrapped in an IIFE) works as written. **No action
needed** — code that previously got the wrong value now gets the right one. If you
adopted an IIFE workaround (`(()=>{ ...; return x })()`), it continues to work.

### New: reach databases outside the app-data directory

If your app stores its SQLite database outside the OS app-data directory (a common
case — project/working dir, a user-chosen path), `query_db` and `introspect db_health`
could not find it. Register the containing directory:

```rust
VictauriBuilder::new()
    .db_search_paths(["../data", "/abs/path/to/data"])
    .build()
```

Configured roots take precedence in auto-discovery, and absolute `query_db` paths are
permitted when they resolve within an allowed root (read-only and traversal-guarded).

### Log tools now apply a default limit

`logs ipc`/`network`/`slow_ipc` now return at most 100 entries by default and truncate
per-entry fields larger than 4 KB. Pass an explicit `limit` for more entries. This
prevents the tools from exceeding the eval size cap on apps with heavy IPC traffic.

## v0.5.5 → v0.5.6

### Breaking Change: Auth Enabled by Default

The MCP server now **generates a Bearer token automatically** on startup and enforces authentication on all endpoints except `/health`. Previously auth was opt-in.

**If you use `VictauriClient::discover()`** — no change needed. The client reads the token from the discovery directory automatically.

**If you use a custom HTTP client** — read the token from `<temp>/victauri/<pid>/token` and send it as `Authorization: Bearer <token>`.

**If you want the old behavior (no auth):**

```rust
VictauriBuilder::new()
    .auth_disabled()   // Explicitly opt out of auth
    .build()
```

**If you set `VICTAURI_AUTH_TOKEN` env var** — that token is used instead of auto-generation. Behavior unchanged.

### Behavior Changes

**DNS rebinding guard** — All requests must have a `Host` header matching `localhost`, `127.0.0.1`, `[::1]`, or `localhost:<port>`. Requests from DNS-rebound hostnames (e.g. `evil.com` resolving to `127.0.0.1`) receive 403. This affects both the plugin MCP server and the browser native host.

**Security response headers** — All responses now include `X-Content-Type-Options: nosniff`, `Cache-Control: no-store`, `X-Frame-Options: DENY`, and `Content-Security-Policy: default-src 'none'`. If your client parses response headers, these are new.

**Eval output limit** — `eval_js` results exceeding 5 MB return an error instead of the result. If you eval expressions that produce very large strings (e.g. `JSON.stringify(document.body)`), you may need to trim output in your JS expression.

**Rate limiter 429 responses** now include a `Retry-After: 1` header. Clients should respect this before retrying.

**`get_diagnostics` env vars** — The environment variable allowlist was trimmed from ~30 to 16 prefixes. If you relied on seeing `PATH`, `RUST*`, `CARGO*`, `APPDATA`, or other system variables in diagnostics output, they are no longer exposed.

**SQL hardening** — `query_db` now strips SQL comments (`--` and `/* */`) before the read-only check and rejects stacked queries (statements with `;`). Legitimate multi-statement queries are not supported.

### Version Bump

```toml
victauri-plugin = "0.5.6"
victauri-test = "0.5.6"
```

---

## v0.5.4 → v0.5.5

### New Public API

**`AppEvent::Console` variant** — Console log events from the bridge are now typed as `AppEvent::Console { level, message, timestamp }` instead of `AppEvent::StateChange { key: "console.warn", caused_by: Some(message) }`. If you match on `AppEvent` variants, this is a new arm. Since `AppEvent` is `#[non_exhaustive]`, existing code with wildcard matches compiles unchanged.

**`AppEvent::is_internal()`** — Returns `true` for Victauri's own infrastructure events (`plugin:victauri|*` IPC). Use this instead of manual string-matching when filtering event logs.

### Behavior Changes

**Bridge ready signal** — The JS bridge now sends a `__victauri_bridge_ready__` callback when it initializes. The eval pipeline waits up to 5 seconds for this signal before the first eval. This eliminates the race condition on page load and removes the 2-second first-call latency from the probe mechanism.

**Discovery session tokens** — The server now always writes a session token to the discovery directory (`<temp>/victauri/<pid>/token`), even when auth is not enabled. `VictauriClient::discover()` reads this token and includes it as a Bearer header. When auth is off, the header is harmlessly ignored. This prepares the path for zero-config auth in a future release.

### Version Bump

```toml
victauri-plugin = "0.5.5"
victauri-test = "0.5.5"
```

---

## v0.5.2 → v0.5.3

### Behavior Changes

**`find_elements` with invalid CSS selectors now returns an error** instead of silently returning `[]`. If your code catches empty results as "not found", you may need to handle the error case:

```json
// Before: {"selector": "###invalid"} → []
// After:  {"selector": "###invalid"} → isError: "invalid CSS selector: ###invalid — ..."
```

**`eval_js` errors now set MCP `isError`** flag. Previously, `throw new Error("x")` returned `{"__error":"x"}` as a successful text result. Now it returns a proper MCP error with `isError: true` and message `"JavaScript error: x"`.

**`eval_js` with `undefined` and `null`** — Previously returned `{}` for both. Now returns `"undefined"` (string) or `null` respectively.

### Fixed

- **Network log pollution:** Victauri's own `plugin:victauri|*` IPC traffic no longer fills the 1000-entry `networkLog` buffer, preserving real app IPC evidence for `get_ipc_log`
- **Hidden window timeout:** Eval targeting hidden/unresponsive windows now fails in 2 seconds with a diagnostic message instead of timing out after 30s
- **Replay after stop:** `recording.replay` and `recording.export` now work after `recording.stop` — session data is persisted
- **Checkpoint label alias:** `recording.checkpoint` now accepts `label` as an alias for `checkpoint_label`
- **Explain noise:** `explain.summary`, `explain.last_action`, and `explain.diff` filter out Victauri's internal IPC and state change events

### Version Bump

```toml
victauri-plugin = "0.5.3"
victauri-test = "0.5.3"
```

---

## v0.5.0 → v0.5.2

### Introspect Action Renames

Three actions renamed for clarity:

| Old Name | New Name | Reason |
|----------|----------|--------|
| `managed_state` | `plugin_state` | Distinguishes Victauri's own state from app state |
| `tasks` | `plugin_tasks` | Distinguishes Victauri's async tasks from app tasks |
| `fs_scope` | *(removed)* | Redundant with `app_info` tool |

### Enhanced Actions

- **`capabilities`** now returns structured security config (CSP, `freeze_prototype`), configured plugins, window definitions, and privacy profile
- **`processes`** now enumerates child processes (sidecars, background workers) with PID, name, and memory usage
- **`event_bus`** events are now captured automatically — apps no longer need to manually push events

### New Builder Method

```rust
VictauriBuilder::new()
    .listen_events(&["notification-added", "settings-changed"])
    .build()
```

Window lifecycle events (resize, move, focus, close, theme, drag-drop) are captured automatically without `listen_events`.

---

## v0.4.x → v0.5.0

### New Tools

Three new compound tools added — no breaking changes, purely additive.

**`introspect`** — 13 actions for deep backend introspection:

```json
{"action": "command_timings", "slow_threshold_ms": 100}
{"action": "coverage"}
{"action": "contract_record", "command": "get_settings"}
{"action": "contract_check"}
{"action": "startup_timing"}
{"action": "capabilities"}
{"action": "db_health"}
{"action": "plugin_state"}
{"action": "processes"}
{"action": "plugin_tasks"}
{"action": "event_bus"}
```

**`fault`** — IPC chaos engineering (delay, error, drop, corrupt):

```json
{"action": "inject", "command": "get_settings", "fault_type": "delay", "delay_ms": 2000}
{"action": "inject", "command": "save_data", "fault_type": "error", "error_message": "disk full"}
{"action": "list"}
{"action": "clear_all"}
```

**`explain`** — Natural-language event narration:

```json
{"action": "summary", "seconds": 30}
{"action": "last_action"}
{"action": "diff", "seconds": 15}
```

**`recording.replay`** — New action on existing `recording` tool. Re-executes recorded IPC commands and checks for response shape drift:

```json
{"action": "replay"}
```

### Version Bump

Update your dependency version:

```toml
victauri-plugin = "0.5"
victauri-test = "0.5"
```

---

## v0.3.x → v0.4.0

### Breaking Change: Auth Disabled by Default

Authentication is now **disabled by default**. Previously, the plugin auto-generated a UUID Bearer token on startup. This caused silent MCP connection failures when the token wasn't configured in `.mcp.json`.

**If you were relying on auto-generated auth**, opt in explicitly:

```rust
VictauriBuilder::new()
    .auth_enabled()  // auto-generates UUID token, printed to console
    .build()
```

**If you were calling `.auth_disabled()`**, you can remove it — auth is already off by default. The method still exists as a no-op for backwards compatibility.

**If you were using `VICTAURI_AUTH_TOKEN` env var or `.auth_token("...")`**, no change needed — those still enable auth.

### New: `register_command_names` Builder API

Lightweight alternative to `#[inspectable]` proc macros:

```rust
VictauriBuilder::new()
    .register_command_names(&["get_settings", "save_settings", "search"])
    .build()
```

### New CLI Commands

- `victauri invoke <command>` — call any Tauri IPC command from terminal
- `victauri doctor` — full setup diagnosis
- `victauri init` now scaffolds CLAUDE.md with agent instructions

---

## v0.2.x → v0.3.0

### New Features

- **Browser extension ecosystem** — `victauri-browser` crate with native messaging host for Chrome/Edge/Brave/Arc/Firefox. MCP inspection for any website, not just Tauri apps.
- **Firefox extension** — full MV3 port using `browser.*` namespace in `extensions/firefox/`
- **npm package** — `@4da/victauri-browser` with postinstall binary download from GitHub releases
- **163 JavaScript tests** — vitest + jsdom test suite for the Chrome extension JS bridge
- **52 E2E Rust tests** — full pipeline integration tests for victauri-browser
- **mdbook documentation site** — 10-page docs site in `docs/`
- **Release workflow** — GitHub Actions pipeline: test gate → cross-platform matrix builds → Chrome extension zip → sequential crates.io publish → GitHub Release with all artifacts
- **CI for JS tests** — Chrome extension vitest job added to CI workflow

### No Breaking Changes

This is a feature-only release. All existing APIs are unchanged.

---

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

#### Privacy profiles replace boolean strict mode

`strict_privacy_mode()` still works but now maps to `PrivacyProfile::Observe`. The new `privacy_profile()` builder method gives finer control:

```rust
// Before (v0.1.x) — boolean, all-or-nothing
VictauriBuilder::new().strict_privacy_mode()

// After (v0.2.0) — three tiers
use victauri_plugin::PrivacyProfile;

// Read-only: snapshots, logs, registry — no clicks, no input, no eval
VictauriBuilder::new().privacy_profile(PrivacyProfile::Observe)

// Testing: observe + interactions + input + storage writes + recording
VictauriBuilder::new().privacy_profile(PrivacyProfile::Test)

// Everything (default, same as v0.1.x)
VictauriBuilder::new().privacy_profile(PrivacyProfile::FullControl)
```

`strict_privacy_mode()` is equivalent to `privacy_profile(PrivacyProfile::Observe)`.

**New tool gates:** `interact` and `recording` are now privacy-gated (blocked in `Observe`, allowed in `Test`). `invoke_command` in `Test` profile requires the command to be on the allowlist.

`disable_tools()` still works as an override layer on top of any profile.

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
