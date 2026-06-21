# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.5] - 2026-06-20

A "fix all known issues before release" pass: ships the `victauri check` tool-count fix that landed
after 0.8.4 was cut, plus a thorough documentation correctness + honesty sweep (two audit agents,
every finding verified against the live code). No public Rust API change.

### Fixed

- **`victauri check` no longer prints `Tools:  ?`.** `get_plugin_info` reports the tool count nested
  at `tools.total`, but `cmd_check` read a flat `tool_count` / bare-numeric `tools` — both miss, so it
  always showed `?`. Extracted a tested `parse_tool_count` (reads `tools.total`, with the legacy shapes
  as fallbacks); 2 unit tests. (Crates: `victauri-cli`.)
- **Documentation code examples that would panic or not compile.** mdbook examples aren't doctested, so
  several had drifted from the API:
  - **Panicking:** `detect_ghost_commands` results were indexed by non-existent keys (`ghost_commands`,
    `ghosts`) then `.as_array().unwrap()` — corrected to `confirmed_ghosts` (README, `testing.md`,
    `testing-tauri-apps.md`).
  - **Won't compile:** `eval_js` REST param `expression` → `code`; the standalone `assert_json_*`
    helpers are synchronous and take `&Value` (were shown as awaited client methods);
    `assert_windows_exist()` takes no args; `assert_dom_complete_under` takes a `Duration`;
    `smoke_test()` / `smoke_test_with_config(cfg)` arity, `SmokeConfig.max_dom_complete_ms` (not
    `max_load_ms`), `SmokeReport::passed_count()/total_count()` (not `.passed`/`.total`);
    `client.checkpoint` / `events_between` / `eval_js_in` / `register_command` don't exist (use the
    `recording` tool / `dom_snapshot_for` / a `webview_label` field / `.commands(&[…])`); every
    `.build()` returns a `Result` (config snippets now `.unwrap()`).
  - **Reference-table fixes:** `query_db` and `check_ipc_integrity` return-key lists corrected to match
    the real output shapes.

### Changed

- **Honest public claims.** "the IPC history has no `eval_js` equivalent" was false (the IPC log is
  derived from the page's own `fetch` traffic) — reframed so the database and command registry are the
  AppHandle-only items, and the IPC log is noted as sharing the webview's fate. "a CDP-class tool can't
  attach at all" on WKWebView/WebKitGTK softened to the accurate "no Chrome DevTools Protocol surface"
  (those engines have their own, separate remote-inspector protocols). The registry-enumeration claim
  now notes it covers the `#[inspectable]` subset plus IPC-log mining. (Supersedes PR #17.)

### Security

Pre-publish adversarial-audit hardening (four red-team passes; no Critical/High found; all changes
internal, no public API or behavior change for normal callers):

- **DNS-rebinding `Host` guard tightened.** `is_localhost_host` rejected a bracketed-IPv6 prefix that
  smuggled a non-localhost authority (`[::1].evil.com`), and accepted any `:`-suffix without validating
  it (`localhost:notaport`, `[::1]:garbage`). It now requires the port to parse as a `u16` and matches
  `localhost` case-insensitively. (Defense-in-depth: the Origin guard already blocked the browser path.)
- **`/health` is now rate-limited.** It was registered after the rate-limit layer (axum applies a layer
  only to routes registered before it), so it was un-throttled; it is now covered while staying
  auth-exempt for liveness probes.
- **`query_db` absolute-path branch opens the canonical validated path** (it previously opened the
  caller's literal path — an asymmetry with the relative branch — narrowing a validate/open TOCTOU).
- **Windows discovery directory: the shared root is ownership-checked before the per-PID directory is
  created**, mirroring the Unix parent-ownership check — defeating a pre-planted/owned shared-root swap
  on a multi-user `TEMP`.
- **Docs/CI honesty + correctness:** redaction reframed as a best-effort lint (not a security boundary);
  "backend tools are read-only" scoped to the introspection tools (`invoke_command` can mutate); the
  Open VSX publish step's always-false `if:` fixed; a drifted CI toolchain pin aligned.

## [0.8.4] - 2026-06-20

CLI↔plugin **version-skew** compatibility fix, driven by an in-the-wild session that drove a live
Tauri app through Victauri. An **old `victauri` CLI (0.5.6)** built for the pre-stateless *stateful*
server aborted the MCP handshake against a **0.8.x stateless** plugin with the cryptic
`no mcp-session-id header` — while wrongly blaming a not-running app (it was running). The cliff is
client-side and old binaries cannot be patched, so the fix is server-side. Additive and semver-clean
(a backfilled response header + clearer diagnostics — no API or output-schema change).

### Fixed

- **Stateless MCP now backfills a constant `Mcp-Session-Id` for old/strict clients.** rmcp's stateless
  Streamable-HTTP transport never emits an `Mcp-Session-Id`; a strict client built for the stateful
  server treats its absence as a fatal handshake error. The `/mcp` route (stateless mode only) now
  emits a fixed sentinel `Mcp-Session-Id: stateless`. It is never validated server-side, so it can
  never go stale → the `422 "expected initialize request"` wedge that stateless mode exists to prevent
  cannot return. Clients may ignore or echo the extra header; both paths are supported. Scoped to
  `/mcp` via a per-route layer, so `/api/tools`, `/info`, and `/health` are unaffected.
- **`victauri doctor` now counts CLI↔plugin version-skew warnings in its summary.** The warning already
  printed, but the final pass/warn/fail counts under-reported it. The connection helper also avoids
  classifying unrelated generic "session" errors as version skew unless the error names the MCP
  handshake symptoms (`Mcp-Session-Id`, `expected initialize request`, or an initialize-time 422).
- **The `victauri-test` GitHub Action now tolerates cold webview startup.** Its `check: true`
  diagnostics retry only while `victauri check` reports the known JS-bridge-not-ready class, so Linux
  WebKit can finish loading after `/health` is already green without masking unrelated failures. The
  action's default `victauri-version` is also version-synced to `0.8.4`.

### Added

- **`victauri check` / `doctor` warn loudly on CLI↔plugin version skew.** When the CLI's own
  `major.minor` differs from the running plugin's, both commands print a non-fatal warning naming the
  cryptic symptom (`no mcp-session-id header`) and the one-line fix (`cargo install victauri-cli
  --force`), including the OS-specific command to kill the stale `victauri bridge` proxy processes that
  hold the on-PATH binary locked during reinstall ("Access is denied" / "Text file busy").
- **Connection-failure diagnostics now match the real cause.** A `401`/`Unauthorized` (auth is on by
  default) and a version-skew handshake failure previously both surfaced as the generic "is your app
  running?" while the app *was* running. `victauri check` now classifies the failure and points at auth
  (discovery token / `VICTAURI_AUTH_TOKEN` / `auth_disabled()`) or a CLI upgrade accordingly.
- **Compatibility guard tests now cover the strict-client path directly.** The stateless transport
  tests prove the `initialize` response carries `Mcp-Session-Id: stateless`, an echoed compat id works
  through `notifications/initialized` and a later tool call, bogus session ids still never 422, auth
  remains enforced, and the compat header does not leak to `/api/tools`, `/info`, or `/health`.

### Changed

- **`bridge not responding` errors now name the page-not-loaded case.** A dev-server
  connection-refused or a blank error page has no JS bridge, so `dom_snapshot` / `eval_js` / console
  tools correctly fail — the message now says exactly that and points at the `screenshot` tool, which
  works regardless of page JS (it is what cracked the in-the-wild white-screen diagnosis).

## [0.8.3] - 2026-06-16

In-the-wild fixes from a live 4DA analysis session driven **entirely through the Victauri
bridge** (REST `/api/tools`, no CDP/Playwright): a full morning-brief + preemption review on
the running app. The 0.8.2 host-crash fix held (bridge enabled, no `VICTAURI_DISABLE`, no
crash). Two genuine, generic friction points surfaced; both fixed here, then hardened by a
GPT-5.5 adversarial audit (which confirmed the omitted-label gap below and found no `query_db`
security regression). Additive and semver-clean (a serde alias + a pre-capture visibility
resolution — no output-schema change).

### Fixed

- **`screenshot` of a non-visible window no longer returns a stale/wrong image silently.** A
  native screenshot captures the on-screen surface; a hidden window has none, so the OS capture
  path (PrintWindow / `CGWindowListCreateImage`) yields stale, empty, or *another window's*
  pixels with no error — and an agent cannot tell a wrong image from a right one. Two paths are
  fixed: **(1)** an explicitly-requested hidden window (e.g. 4DA's hidden `briefing` panel
  returned the *main* window's pixels) now returns a clear, actionable error; **(2, GPT audit
  P2)** `screenshot {}` with **no label** previously resolved through `find_window(None)`, which
  prefers `"main"` *unconditionally* — so an app that hides main but leaves a secondary window
  visible captured hidden main. The tool now resolves its own visible target (prefer a visible
  `main`, else the first visible window, else a clear "no visible window to capture" error) and
  passes it explicitly to the OS-handle lookup. `find_window` itself is unchanged — other callers
  (e.g. `eval_js`) may legitimately target a hidden window.

### Changed

- **`query_db` accepts `sql` as an alias for the `query` field.** Passing the intuitive `sql`
  key previously failed with an opaque HTTP 400 and no hint that the field is named `query`. The
  alias removes the paper-cut; the tool description now states the field name explicitly.

## [0.8.2] - 2026-06-15

### Fixed

- **Host-process crash under webview reload — the real fix.** 0.8.1's main-thread dispatcher
  *reduced* but did **not eliminate** the crash: it is a Tauri-runtime `Rc<Webview>` use-after-free
  (`STATUS_*_BUFFER_OVERRUN`) that fires when an IPC request hits `tauri::ipc::protocol::get`
  **during a webview reload** (HMR / navigation, preceded by Tauri's "app reloaded while running an
  async op" warnings). Victauri does not cause that Tauri race, but it **amplified** it: the
  background event-drain loop eval'd `getEventStream` in *every window every second* — each eval
  injects JS that calls back over IPC (`victauri_eval_callback`) — so a multi-window app saw a
  constant stream of IPC requests, forever, even when idle, every one of which is a chance to land
  in a reload window. (0.7.11 drained only the default window ≈ 1/sec and was effectively clean;
  0.8.0 made it per-window ≈ N/sec and crashed every few cycles.) **0.8.2 gates the drain loop on an
  active time-travel recording**, so idle introspection produces zero background eval-callback churn
  — removing the dominant amplifier. New regression test reloads the webview while introspecting
  concurrently (the actual trigger the 0.8.1 test failed to exercise).

### Changed

- **`explain` and the JS-event side of `event_bus` now reflect events only while a recording is
  active.** Continuous passive event capture was inseparable from the constant IPC churn above, so it
  is now tied to `recording start` (its natural scope as a session-narration tool). `event_bus`'s
  native Tauri window/lifecycle events are unaffected and always captured.

## [0.8.1] - 2026-06-14

### Fixed

- **Window-target param is no longer silently ignored.** `eval_js`/`snapshot`/etc. accept a
  `webview_label`; `screenshot`/window-management accept a `window_label`. An agent that passed the
  intuitive `window` (or the *other* tool's spelling) hit a serde unknown-field drop → `None` → the call
  always landed on the MAIN window, so multi-window apps (e.g. 4DA's briefing/notification windows) still
  needed CDP. Every webview/window tool now accepts `window`, `window_label`, and `webview_label`
  interchangeably (`#[serde(alias = …)]`, backward-compatible — output schemas unchanged). Found in the
  2026-06-14 live 4DA dogfood of 0.8.0. Regression tests cover `eval_js`, `snapshot`, and `screenshot`.
- **Database introspection is now bounded and root-contained.** `introspect db_health` no longer
  accepts relative `../` escapes or redirecting filesystem entries outside configured roots, no
  longer executes the side-effecting `wal_checkpoint` PRAGMA, and safely quotes arbitrary SQLite
  table names. `query_db` now runs on a blocking worker with a real CPU deadline plus cell/result
  byte caps; `db_health` has the same deadline and a bounded table listing. Exact-`max_rows` results
  are no longer falsely marked truncated.
- **All MCP and plugin-command webview/window access now stays on Tauri's main thread.** Background
  access to `AppHandle::webview_windows()` could race Tauri's non-atomic webview handle storage and
  crash the host process under concurrent real IPC. A shared main-thread dispatcher now covers
  eval, state/list, native handles, and window-management operations, with a live stress regression.
  The dispatcher also runs its closure under `catch_unwind`, so a panic on the UI thread becomes an
  error instead of aborting the host process (and the caller fails fast rather than waiting out the
  timeout).
- **Pure Wayland screenshots fail safely instead of capturing the full desktop.** Linux X11 and
  XWayland retain per-window capture; when that path is unavailable Victauri now returns an
  actionable error rather than leaking unrelated windows through the old `grim` fallback.
- **Consumer CI examples no longer track mutable `main`.** README/docs now reference
  `4DA-Systems/victauri/.github/actions/victauri-test@v0.8.1`, matching the already-pinned
  generator and composite action.
- **Adversarial-audit hardening (concurrency + filesystem).** From a deep pre-publish audit: the
  main-thread dispatcher uses `block_in_place` so blocking on the UI reply never starves the embedded
  server's runtime workers, and skips a mutation whose caller already timed out (no "spontaneous"
  window change after a reported failure). The `query_db`/`db_health` CPU deadline is now a hard
  wall-clock interrupt (the opcode-sampling progress handler under-counts a single long scan).
  `read_app_file` does its file IO on a blocking worker and opens the canonical validated path (a
  swapped FIFO can't stall the executor, and the open matches what containment approved). Trusted
  (OS-level) native input no longer blocks a runtime worker.

## [0.8.0] - 2026-06-14

Driven by the scale-gauntlet cross-engine net and an exhaustive live sweep of 4DA
(379 commands, 747 MB SQLite), then **validated end-to-end against 4DA rebuilt from
source** (every fix proven inside the live process), and hardened by a GPT-5.5 adversarial
audit before release.

**Breaking (minor-version bump under 0.x).** The tool-OUTPUT schemas and runtime behaviour
are backward-compatible — agents/MCP clients need no change. The break is a Rust public-API
cleanup: internal MCP protocol types are no longer re-exported, so a path-dependency on
`victauri-plugin` must move from `"0.7"` to `"0.8"`. See MIGRATION.md.

### Changed (breaking)

- **MCP tool *parameter* types are no longer public API.** The `*Params` / action enums
  (e.g. `IntrospectAction`, `EvalJsParams`) were re-exported from `victauri_plugin::mcp::*`;
  they are an internal protocol surface deserialized from JSON, used only by this crate's
  private tool methods, and they change every release. They are now `pub(crate)`, so adding
  a tool action or field is no longer a (mechanical) breaking change and `cargo
  semver-checks` stays meaningful for the plugin. No consumer used these types.
- **`introspection::TimingSamples` is now crate-private** and the unbounded `pub samples:
  Vec<Duration>` field is gone — replaced by a bounded ring buffer (fixes an unbounded
  per-command memory growth). `CommandTimings` and the other `introspection` types consumers
  actually use remain public.
- Consumers: bump the dependency requirement `victauri-plugin = "0.7"` → `"0.8"` (and
  `victauri-test`). Nothing else changes.

### Added

- **`introspect command_catalog`** — mines the live IPC log for each command's argument
  and result *shapes* (inferred in JS so response bodies never leave the webview), merged
  with the `#[inspectable]` registry. Gives an agent real call/return schemas even when
  the app does not use `#[inspectable]` (where `get_registry` returns names with null
  schemas). Observed commands carry `call_count` / `error_count` / `last_status` plus
  inferred `arg_shape` / `result_shape`; registry-only commands appear as `observed:false`.
  `get_registry`'s description now points to it. (Live on 4DA: 34 observed commands with
  real nested schemas.)

### Fixed

- **Cross-engine bridge correctness (WebKit).** Fixed Chromium/`WebView2`-only JS-bridge
  assumptions that were silently wrong on `WKWebView` (macOS) / WebKitGTK (Linux): the IPC
  scheme (`ipc://` vs `http://ipc.localhost`) and the perf APIs (`performance.memory` /
  longtask / paint). A new cross-engine gauntlet (`examples/gauntlet-app` + battery) is a
  **required CI gate on Linux and macOS** to prevent regressions.
- **Bridge resilience.** The liveness probe now runs before every eval, including the
  **default (unlabeled) window** — previously only labeled windows were proactively
  probed, so the most common path hung the full eval timeout (~30 s) on first contact with
  a reloaded/unready bridge. It now fails fast (~2 s) with a clear message. A hard
  pending-eval capacity check runs **before** the probe.
- **Database reachability.** Relative `db_search_paths` (e.g. `../data`) now resolve
  against the launch CWD **and every executable ancestor**, so `query_db` /
  `introspect db_health` find the database regardless of the directory the app was launched
  from (binaries usually run from `target/debug/`). Live-proven: reached the 4DA database
  from a `target/debug` CWD that defeats the old CWD-only resolution.
- **Three correctness-under-load defects** surfaced by the robustness battery.

### Security

- **`introspect command_catalog` is gated per-action.** It maps to the
  `introspect.command_catalog` capability (FullControl-only, like its sibling introspection
  actions) and is pinned in the exhaustive `AUTHZ_SPEC` test, so
  `disabled_tools: ["introspect.command_catalog"]` is honored — closing a per-action
  authorization gap (the action would otherwise fall back to the bare `introspect`
  capability) before it shipped.

### Internal

- Plugin test suite ~10–15 min → ~40 s: four concurrent tests shared one stateful MCP
  session whose SSE responses stalled `resp.text()` ~300 s each; moved to per-task
  sessions. 499 plugin tests green; clippy `--all-targets` / `--no-default-features` /
  `--release -Dwarnings` / `fmt` clean across the matrix.

## [0.7.11] - 2026-06-08

Driven by a real session driving **live 4DA**'s embedded Victauri 0.7.10. Six issues
(VIC-1..6) were filed against Victauri; all are addressed here. Additive / bugfix — **no
breaking output-schema change** (Semver Checks green), so `"0.7"` consumers pick it up.

### Fixed

- **`detect_ghost_commands` is now OUTCOME-based, not a registry diff (VIC-1, core-value).**
  It used to diff frontend-invoked commands against Victauri's `#[inspectable]` registry — an
  incomplete subset of the real `tauri::generate_handler!` set (which Tauri exposes no runtime
  API to enumerate) — so every real-but-uninstrumented command (4DA's `set_language`) and every
  framework `plugin:*` builtin was reported as a ghost; 0.7.10 only added a caveat. It now keys
  on the IPC **outcome**: a command that returned success at least once **provably has a
  handler** (`verified_handlers`, never flagged — this is what excludes `set_language`); a
  command that only ever errored "not found" is a `confirmed_ghosts` entry (high confidence,
  registry-independent); `plugin:*` commands are `excluded_builtins`. The legacy `frontend_only`
  remains as a much-tighter weak-candidate tier (invoked, never succeeded, not a builtin, absent
  from the registry). Additive output fields; 5 regression tests.
- **`get_diagnostics.bridge_version` no longer drifts (VIC-2).** The JS bridge's self-reported
  version was a hand-maintained literal that the bump script find-replaced each release; it
  silently stuck at `0.7.8` through 0.7.10, so `get_diagnostics` reported a stale `bridge_version`
  on a fresh process and the startup self-check logged a false "Bridge version mismatch" every
  launch. `init_script()` now injects `env!("CARGO_PKG_VERSION")` into a placeholder, so the JS
  bridge version is **always** the crate version (matching the Rust `BRIDGE_VERSION` const) and
  cannot drift again. The bridge tests assert this equality.
- **`resolve_command` no longer returns opaque N-way ties (VIC-3).** When command metadata was
  absent, several commands tied on score and came back in arbitrary (HashMap) order. Added a
  deterministic name tiebreak plus a small name-coverage specificity bonus, so ranking degrades
  gracefully (a query word hitting the short `settings` outranks the same word buried in
  `get_app_settings_v2`).
- **`introspect event_bus` is capped (VIC-4).** It dumped the full Tauri + app event buffers (up
  to ~11k events / ~1.68 MB / tens of thousands of lines), overflowing the tool result cap. It
  now returns the newest `limit` events per category (default 100) with the true `count` and a
  `truncated` flag, and accepts `limit` / `since_ms` to scope output.
- **Clearer MCP-restart diagnostics (VIC-5).** The 422 stale-session class was already eliminated
  by 0.7.10's stateless-by-default transport, and `victauri bridge` already auto-reconnects on an
  app restart. The remaining rough edge — an opaque "server unreachable" while the app rebuilds —
  now returns an actionable message explaining the app is likely restarting and the bridge
  reconnects automatically.

### Changed

- **`invoke_command` args contract clarified (VIC-6).** Investigation confirmed the MCP and REST
  paths are identical (same `InvokeCommandParams`, same `execute_tool` dispatch and forwarding),
  so a "missing argument" via MCP but not REST is a caller passing args flat instead of nested
  under `args`. The tool/param description now states the shape explicitly
  (`{"command":"…","args":{…}}`). No behavior change.

## [0.7.10] - 2026-06-07

Driven by an in-the-wild session that drove **live 4DA** over the REST bridge and
documented five frictions. The two that were real, generic correctness bugs (verified
against HEAD, not the session summary) are fixed here — live-verified against the
demo-app and green across the full multi-platform CI incl. Live-App Proof on all three
OS. All changes are **additive / bugfix — no breaking output-schema change** (Semver
Checks green), so consumers on `"0.7"` (e.g. 4DA) pick them up with no requirement
change.

### Fixed

- **`introspect command_timings` is no longer blind to the app's real frontend IPC.**
  It only ever recorded commands driven through Victauri's own `invoke_command` tool,
  so it reported `0 profiled` while a live app made hundreds of real calls. It now
  *also* derives per-command `call_count` + min/max/avg/p95 latency from the live IPC
  log (new `ipc_traffic` field) — the figure that reflects actual usage — alongside a
  `note` making the two sources explicit. `ipc_commands_observed` is also reported.
- **`introspect coverage` no longer reports "0 invoked" on busy apps.** It eval'd the
  full `getIpcLog()` *with* request/response bodies, which exceeds the 5 MB eval-result
  cap on a heavy-traffic app → empty parse → `0 invoked` despite live traffic (the same
  busy-app failure mode fixed for `logs`/ghost-detection on 2026-05-29, which had missed
  this call site). Switched to the body-free command-name projection; added
  `ipc_calls_observed` so a zero is now diagnosable.
- **`detect_ghost_commands` no longer over-claims.** `frontend_only` means "absent from
  Victauri's `#[inspectable]` introspection registry" — a strict *subset* of the real
  `tauri::generate_handler!` set — but the docstrings and tool description claimed "no
  backend handler", so any app not fully using `#[inspectable]` (e.g. one with an empty
  registry) had every real command flagged as a ghost. Corrected the wording and added
  a purely-additive `reliability` (`none` / `low` / `high`) + plain-language `note`.

### Changed

- **`detect_ghost_commands` output is enriched (additive).** Adds `reliability` + `note`;
  the existing `frontend_only` / `registry_only` / totals fields are preserved verbatim.
- **`victauri-cli doctor`** and the **`victauri-test` `NoGhostCommands`** smoke check are
  now reliability-aware — they no longer treat `frontend_only` entries as bugs unless the
  registry is complete (`reliability: high`). (`NoGhostCommands` also had a latent
  always-passes bug — it read a `ghost_commands` key that never existed — now fixed.)
- **The MCP `/mcp` transport now runs STATELESS by default** (rmcp's default is stateful).
  Stateful mode minted an in-memory `Mcp-Session-Id` at `initialize` that dies on app
  restart / idle eviction / SSE-stream drop, after which rmcp answers `422 "expected
  initialize request"`. Because `422` is non-standard for an expired session (the spec uses
  `404`), a generic MCP client — including the agent harness that speaks rmcp directly and
  cannot use the recovering `victauri bridge` — never re-initializes and stays wedged for the
  whole run, forcing the sessionless REST API for everything. Stateless mode has no session
  to lose, so that 422 class cannot occur, and responses are returned as plain
  `application/json`. All 35 request/response tools and one-shot `resources/read` are
  unaffected. Stateless drops only the long-lived server-push SSE channel — and Victauri
  never actually implemented resource-update push (subscribe/unsubscribe only recorded
  intent; nothing emitted notifications), so no working feature is lost. The hollow
  `resources.subscribe` capability is no longer advertised. `build_app_stateful` keeps the
  stateful session transport available for clients that require the session protocol. The
  `victauri-test` client and `victauri bridge` handle either mode transparently.

### Security

- **A blank auth token no longer disables auth — the contract is now uniformly
  "auth on by default unless `auth_disabled()`".** v0.7.9 *normalized* an empty or
  whitespace `auth_token("")` / `VICTAURI_AUTH_TOKEN=""` to **no auth** (a silent
  fail-open: a botched env var or build script could turn auth off without anyone calling
  `auth_disabled()`). The plugin builder's `resolve_auth_token` now treats a blank
  configured token as *unset* and **generates a real auto-token** instead, so auth stays
  on unless the operator explicitly opts out. The CLI `victauri bridge` got the matching
  client-side fix: on the `VICTAURI_PORT` override path a blank `VICTAURI_AUTH_TOKEN` is
  treated as unset and falls through to the token discovered for that exact port, rather
  than sending an empty Bearer that would 401 every call (`normalize_env_token`). This
  reverses the v0.7.9 "empty tokens normalized to no auth" behavior noted below.
- **Discovery directory is now locked to the current user with a true owner-only DACL on
  Windows (round-4 audit #4).** The previous `icacls /inheritance:r /remove … /grant:r`
  hardening stripped inherited ACEs and the common world/group principals, but a *pre-planted
  explicit ACE for an arbitrary principal* (the auditor's example: `BUILTIN\Guests`) survived,
  because `/grant:r` replaces only the owner's ACE. Victauri now (1) **verifies the directory
  is owned by the current user** before trusting it — refusing an attacker-pre-planted dir on
  a shared `TEMP` (the Windows counterpart to the existing Unix uid check) — and (2) **replaces
  the DACL with a PROTECTED owner-only DACL** via the Win32 security API
  (`SetNamedSecurityInfoW`), so no inherited or pre-existing explicit ACE can survive. The old
  `icacls` path remains only as a logged fallback if the API call fails on an unusual
  filesystem. Applied to both the plugin server and the browser native host; proven by a test
  that plants a `Guests` ACE and asserts it is gone after the lockdown. (The plugin is
  `#[cfg(debug_assertions)]`-gated and the default per-user `TEMP` is not writable by other
  users, so the prior residual was never exploitable on a default setup — this closes it for
  non-default shared-`TEMP` layouts too.)
- **`victauri bridge` discovery now verifies directory ownership before trusting a token**
  (audit #15, read side). The bridge — the stdio MCP proxy Claude Code connects through —
  scanned `<temp>/victauri/<pid>/` and read the Bearer token after only a process-liveness
  check. On a shared-temp Unix host a local attacker could plant a fake `<pid>` directory
  (named after one of their own live processes) pointing at a server they control and
  harvest the real token (and feed forged tool results). `discover_servers` now applies the
  same `dir_is_trusted` guard `victauri-test` already used: a directory is trusted only if
  it is a real directory (not a symlink), owned by the current uid, and not group/other-
  writable. No effect on Windows (per-user temp). Found by the pre-release internal sweep.

#### Red-team audit hardening (external cross-model adversarial pass)

A cross-model (GPT/Codex) adversarial audit of the pre-release `b9fd6de` found, and this
release fixes, the following — each independently reviewed and verified against the code:

- **A4 browser channel — WebCrypto key-import hook (critical).** The MAIN-world bridge
  imported its HMAC key lazily via the page-controllable global `crypto.subtle`, with the
  raw nonce as key material — a hostile page that patched `crypto.subtle.importKey`/`sign`
  at `document_start` could steal the nonce and defeat the A4 authentication. The bridge now
  captures pristine `crypto.subtle`/`TextEncoder`/`JSON`/`Promise` references at
  `document_start` (before page scripts run) and imports the key eagerly during the nonce
  handshake. (Browser extension — EXPERIMENTAL, separate cadence.)
- **A4 command replay + signed-payload TOCTOU.** Authenticated command ids are now consumed
  before execution (a page replaying a valid command event can't repeat side effects), and the
  exact MAC'd args are snapshotted as JSON and executed as an immutable copy (the shared event
  detail was page-mutable during the async MAC verification). Mirrored in both worlds.
- **Weak nonce fallback.** Without a CSPRNG the ISOLATED relay fell back to a guessable
  `Date.now()+Math.random()` nonce; it now returns `null` and the channel fails closed.
- **Cross-port auth-token leak.** On the `VICTAURI_PORT` override path the bridge paired the
  *first* token found among any running app with the requested port — sending one app's Bearer
  token to an unrelated localhost port. Token is now matched to its exact port (`token_for_port`).
- **Unix symlink-clobber + discovery-token disclosure.** Discovery-dir setup chmod'd *through*
  a planted symlink and wrote the token through it; uid probes used symlink-following
  `fs::write`. Now refuses symlinked/untrusted paths, uses `O_EXCL` probe files, never chmods a
  symlink target, and won't `remove_dir_all` a directory it does not own. Applied across the
  plugin server, the CLI bridge, the test client, and the browser host discovery.
- **Release pipeline / supply chain.** SHA-pinned the remaining floating GitHub Actions
  (`docs.yml`, `surface-audit.yml`); added a protected `environment: release` to the npm and
  VS Code publish jobs; removed the `|| npm install` lockfile-bypass fallbacks (`npm ci`
  everywhere); switched credentialed publishes to `npx --no-install`; and the crates publish
  job now additionally `needs` the release-binary build and the packaged extension before the
  irreversible publish.

## [0.7.9] - 2026-06-07

Driven by the agent-eval A/B (`scripts/agent-eval/RESULTS.md`), which refuted the
naive "browser tools can't reach the Rust backend" thesis and surfaced the honest,
defensible one — *browser tools can **poke** a Tauri backend, but only Victauri can
**read** it safely: the database, the command registry, and the IPC history have no
`eval_js` equivalent, the backend tools run read-only and webview-independent, and on
macOS/Linux a CDP-class tool can't attach at all.*

### Added

- **`detect_ghost_commands` gains a `since_ms` time window.** Pass `since_ms` (e.g.
  `5000`) to scope ghost detection to commands invoked in the last N milliseconds —
  the non-destructive per-test pattern (invoke the suspect action, then detect). The
  eval found the session-persistent IPC ring buffer buried a true-positive ghost in
  stale probe traffic; this lets an agent get a clean signal without `logs
  {action:'clear'}` (which wipes the log for every reader). New
  `VictauriClient::detect_ghost_commands_since`. The cutoff is evaluated in the
  webview's own clock, so there's no Rust↔JS skew.

### Changed

- **Documentation honesty.** The FAQ, introduction, and project pitch no longer claim
  "Playwright sees only the browser glass" (the eval showed any `eval_js`-capable tool
  can invoke Tauri commands via `window.__TAURI_INTERNALS__.invoke`). They now lead
  with the validated *poke-vs-read-safely* thesis and the genuine moat: read-only
  safety, the no-`eval_js`-equivalent capabilities (`query_db`, registry enumeration,
  IPC history with bodies, native process state), cross-platform reach, and
  webview-independent robustness.
- **Honest release-cost claim (audit B6).** "Compiles away to nothing / zero binary
  size overhead" is corrected to **zero *runtime* cost** — the server is
  `#[cfg(debug_assertions)]`-gated so `init()` is a no-op in release, but the crate
  still compiles in (add it as a `dev-dependency` for zero binary footprint).
- **Firefox scope (audit C4):** the native-host installer registers Chromium browsers;
  the Firefox MV3 port's native-messaging manifest must be registered manually.

### Security

Response to a two-pass cross-model (GPT-5.5) adversarial audit. The keystone was a
structural authorization bypass; the rest hardens the real machine-touching surfaces
(npm, the browser native host, and CI), since the in-app plugin is a no-op in release.

- **Centralized action-level authorization (audit A3/B4 — the keystone).** The
  dispatchers gated only on the bare tool name, leaving per-action enforcement to each
  handler — so actions whose handler forgot to check (e.g. `route.clear`/`clear_all`)
  were reachable even when an operator disabled them, and the Test profile's per-action
  matrix entries were unreachable. A new `mcp::authz::canonical_capability` resolves the
  authoritative `tool.action` identity, and both the REST and MCP dispatchers gate on
  `PrivacyConfig::is_call_allowed(bare_tool, capability)` before dispatch. `recording.
  replay`/`flush` are now FullControl-only; `verify_state`/`assert_semantic` (arbitrary
  eval) are correctly removed from the Test profile. A new **negative dispatch test
  suite** proves through the real dispatch path that blocked actions cannot execute.
- **MCP resources now honor privacy (audit B1).** `read_resource`/`subscribe` applied
  redaction but no access control — a strict profile that blocked log/window reads as
  tools could still read them as resources. They now apply the same capability gate.
- **Empty auth tokens rejected (audit B2).** A `Some("")`/whitespace token enabled the
  auth middleware while reporting `auth_required: true` and accepting an empty Bearer.
  Empty tokens are normalized to "no auth" with a loud warning. **(Superseded in 0.7.10
  — see above: a blank token now generates a real auto-token instead of disabling auth,
  so the contract is uniformly "auth on by default unless `auth_disabled()`".)**
- **`query_db` PRAGMA allowlist (audit C10).** Side-effecting PRAGMAs (`wal_checkpoint`,
  `optimize`, `incremental_vacuum`) are now rejected even without an `=`, on top of the
  existing READ_ONLY open flag and write-form block.
- **Port-fallback overflow (audit C7):** `try_bind` no longer overflows `u16` when the
  preferred port is near 65535 (`checked_add`).
- **Browser native host (audit B5/C3/B9/C5):** stop logging the full bearer token (add
  a user-only discovery file); require a real extension id on install (no silent
  `EXTENSION_ID` placeholder); reap pending dispatch entries on write failure; add an
  HTTP concurrency limit; npm postinstall fails loud instead of silently exiting 0.
- **npm install no longer modifies the system without consent (audit #1):** the
  postinstall no longer auto-registers the native-messaging host (browser manifests +
  Windows registry) on every install — opt in with `VICTAURI_BROWSER_AUTO_REGISTER=1`
  or run `npx victauri-browser install <ext-id>`.
- **Browser extension channel forgery closed (audit A4).** The ISOLATED↔MAIN content-script
  channel shipped the secret nonce inside every `__victauri_command` event on the shared
  `window`, so a hostile page could read it and race a forged `__victauri_response` (the relay
  matched responses by id only) — verified exploitable in real Chromium. Commands AND responses
  are now authenticated with an HMAC-SHA256 keyed by the nonce, which is exchanged only in the
  synchronous `document_start` handshake and never placed on a post-page event; the relay/bridge
  ignore any message without a valid MAC (and fail CLOSED on non-secure http:// origins, where
  Web Crypto is unavailable). Closes both response forgery and command injection; re-verified in
  real Chromium. Chrome + Firefox; reusable `extensions/chrome/tests/e2e/a4-channel-forgery.mjs`.
  Browser mode remains **experimental** (no per-domain/tab privilege model yet); prefer the Tauri
  plugin path for the strongest guarantees.
- **Browser extension now loads.** The manifests referenced `icons/icon-*.png` that did not
  exist in the repo and weren't generated at build, so Chrome refused to load the unpacked
  extension. Added the icon set (Chrome + Firefox) and bumped both manifests off the stale 0.7.1.
  Fixed the Firefox `strict_min_version` to 128 (the `world: "MAIN"` content script the bridge
  needs is only enabled by default in Firefox 128 — it was silently broken on the declared 109).
- **Release/CI hardening (audit A6/C6/D8/#8):** the crate-release workflow only triggers
  on `v[0-9]+.[0-9]+.[0-9]+` tags (a `vscode-*` tag can't fire the crates.io publish);
  third-party actions pinned to commit SHAs; the GitHub Release stays gated on the
  crates publish + `require-ci-green`.
- **Watchdog DoS bounds (audit B12):** clamp the poll interval (≥1s) and failure
  threshold (≥1) so a zero value can't busy-loop / recover every poll; stop logging the
  full recovery command line.
- **Dependencies (audit D10):** verified vitest (4.1.8) and tmp (0.2.7) already patched;
  added the missing `extensions/npm` lockfile; remaining cargo-audit advisories are all
  transitive through tauri/wry/notify with no fix available (documented).

## [0.7.8] - 2026-06-06

A build-correctness release: `victauri-plugin` now compiles in configurations our
CI never exercised (it only ever built default-features + debug). The 0.7.7 compat
retest — which builds the plugin as a `default-features = false` path dep — surfaced
the whole class.

### Fixed

- **`victauri-plugin` failed to compile with `default-features = false`.**
  `query_db`'s `#[cfg(feature = "sqlite")]` gating broke the rmcp `#[tool_router]`
  macro (cfg isn't evaluated at macro-expansion time), so the crate failed with two
  `E0599`s. `query_db` is now always registered as a tool; the SQLite implementation
  moved to a `sqlite`-gated `query_db_impl`, and without the feature the tool returns
  a clear "compiled without the sqlite feature" error. Consumers that drop the heavy
  rusqlite C dependency via `default-features = false` can now build.
- **`cargo test --release` failed to compile** — a unit test referenced a
  `#[cfg(debug_assertions)]` helper; the test is now gated the same way.
- **`RUSTFLAGS=-Dwarnings cargo build --release` failed** on unused imports/constants
  — they're used only by the debug-gated MCP server (`init()` is a zero-cost no-op in
  release), so the release path now allows that intentionally-dead code.

### Changed (CI / internal)

- CI now builds the configurations consumers actually hit so this class can't regress:
  `clippy --no-default-features` and `clippy --release` (both `--all-targets`,
  `-D warnings`).
- The compatibility retest harness now authenticates (sends the Bearer token), so its
  smoke battery actually runs — it scored Kanri 15/15 against 0.7.7. (Harness only;
  not part of the published crates.)

_(No API or behavior changes for consumers on the default configuration. npm
`@4da/victauri-browser` and the VS Code extension are unchanged — crates-only.)_

## [0.7.7] - 2026-06-05

A focused single-fix release closing the one residual from 0.7.6.

### Fixed

- **`victauri test` smoke suite failed on headless CI.** `assert_screenshot_ok` relied on
  `screenshot()` returning `Ok` even when the tool errored; the 0.7.6 `isError` fix correctly
  stopped swallowing tool errors, so on a headless runner (no native/X11 window handle to
  capture) the screenshot check began failing ("1 of 11 checks failed"). It now tolerates the
  headless "no window handle" tool-error — its documented intent — while still failing on
  transport/connection errors. Apps with a real display are unaffected (screenshot returns image
  data as before).

### Changed (CI / internal)

- The repo's own E2E job now builds and tests the **local** `victauri-cli` instead of
  `cargo install`-ing the published crate, so CI validates the code being released rather than
  the previous version (added an opt-in `cli-source-path` to the `victauri-test` composite
  action; external consumers are unaffected and still install the published crate).

_(No API or behavior changes for consumers. npm `@4da/victauri-browser` and the VS Code extension
are unchanged in 0.7.7 — the fix is crates-only.)_

## [0.7.6] - 2026-06-05

Driven by an in-the-wild session that used Victauri to debug a real app's scoring
pipeline (fire-and-forget background work over a large live backlog). The job
succeeded, and the honest teardown named the friction. This release closes the
real, generic gaps it surfaced. All additive — `^0.7`-compatible.

### Added — async-completion awareness (the #1 friction)

Fire-and-forget Tauri commands return instantly while work runs on a background
task, leaving an agent to hand-poll or guess with sleeps. `wait_for` gains two
conditions to await **true** completion:

- **`expression`** — polls a JS expression every interval until it is truthy (or
  equals `expected`). It may `await`, so you can await a status command directly:
  `wait_for { condition: "expression", value: "(await window.__TAURI_INTERNALS__.invoke('get_status')).running === false" }`.
  Level-triggered and race-free; **no app changes required**. Evaluated server-side
  via the same engine as `eval_js` (CSP-safe).
- **`event`** — blocks until a named Tauri event fires, evaluated server-side
  against the captured event bus with a `since_ms` look-back (default 2000) so an
  event emitted in the gap after `invoke_command` is not missed. Custom events must
  be captured via `VictauriBuilder::listen_events(&["…"])`.

New `VictauriClient` helpers: `wait_for_expression`, `wait_for_event`.

### Added — `app_state` tool + state probes (first-class app internals)

Reading domain state (a pipeline's version, a queue's depth, cache stats) used to
mean `query_db` + grepping logs. Apps can now register probes —
`VictauriBuilder::probe("name", || json!({ … }))` — and agents read them through the
new **`app_state`** tool (no args lists probe names; `{ probe: "name" }` returns its
JSON snapshot). Probes run in the Rust process with **no IPC round-trip and no
frontend involvement** — the direct-backend introspection a browser-external tool
cannot do. New `VictauriClient::app_state`. (**25 standalone/compound tools** now.)

### Added — actionable connection diagnostics

A failed connection used to surface as a bare "connection refused", indistinguishable
from a crash, a never-started app, or a backend mid-rebuild. `VictauriClient::discover`
now classifies the discovery directory **before** stale-entry cleanup and attaches a
diagnosis: *stale process* (app exited — crashed/closed/rebuilding; check your build
terminal), or *none* (app not running, or a release build where Victauri is
debug-gated). `victauri doctor` surfaces the same diagnosis instead of a flat "[SKIP]".

### Added — demo-app `run_pipeline` / `pipeline_status` + `pipeline` probe

The demo app now mirrors the real fire-and-forget scenario: `run_pipeline` returns
immediately and emits `pipeline-complete` when its background thread finishes;
`pipeline_status` exposes a pollable status; a `pipeline` state probe reports
`{ pipeline_version, processed, running }`. End-to-end tests await completion via
both the event and the expression path and read state via `app_state`.

### Changed — agent guidance (`victauri init` CLAUDE.md)

The generated agent block now teaches awaiting async backend work with
`wait_for` (`expression`/`event`) instead of sleeps, reading app internals via
`app_state` probes, driving specific code paths through `invoke_command`, and that
`query_db` is read-only by design (mutate via the app's own commands).

### Fixed — GPT-5.5 adversarial red-team pass

A cross-model adversarial audit of this release surfaced real, mostly pre-existing
defects (several stale findings were verified and dismissed). Fixed:

- **`VictauriClient` swallowed MCP tool errors** — `call_tool` returned a tool's
  failure text as `Ok(...)` because it ignored `result.isError`. A failed `eval_js`
  (`throw`), an invalid selector, or any `tool_error` now correctly surfaces as
  `Err(TestError::ToolError(..))`. (Aligns the SDK with the existing
  `regression_eval_throw_returns_mcp_error` / `high_level_api` expectations.)
- **The read-only `Observe` privacy profile was not read-only** — `inspect.highlight`
  and `inspect.clear_highlights` inject/remove DOM overlay nodes, and `logs.clear`
  erases captured IPC/network evidence. These mutating sub-actions were listed but
  never enforced (their handlers skipped the permission check). They are now enforced
  and excluded from `Observe` (still allowed in `Test`/`FullControl`).
- **Tool-invocation metric double-counted** — both transport chokepoints
  (REST `execute_tool`, MCP `ServerHandler::call_tool`) increment `tool_invocations`,
  yet ~19 handlers *also* incremented via a redundant `track_tool_call()`, inflating
  the count on every transport. Removed the per-handler calls; each call now counts
  exactly once, and *all* tools are counted (not just the ones that opted in).
- **`release.yml` could cut a GitHub Release without a successful crates.io publish** —
  `github-release` now `needs: [build, chrome-extension, publish]`.
- **Coverage reported 100% for an empty registry** — a no-commands run now reports 0%
  (unmeasurable, not falsely "fully covered"); the CLI still emits its explicit
  "no commands registered" warning.

## [0.7.5] - 2026-06-02

Two adversarial red-team passes (cross-model) before release. The first pass found
9 issues; the second confirmed all 9 fixed and surfaced a real intent-ranking bug
plus three UX footguns. All fixed below.

### Fixed — red-team audit (first pass)

- **macOS bridge discovery** — `victauri bridge` used a Linux-only `/proc/{pid}` liveness check, so on
  macOS it found *no* running server (the feature was non-functional on macOS). Replaced with a portable
  POSIX `kill -0` check that works on macOS + Linux. (Caught by CI after 0.7.4 — see the new
  `require-ci-green` release gate.)
- **`eval_js` recovery test** — a demo-app adversarial test asserted the old 30s-timeout behavior for a
  syntax error; updated to the fast parse-failure + clean-recovery behavior introduced in 0.7.3.
- **Stale `BRIDGE_VERSION`** — the const was frozen at `"0.5.0"` (the bump script never touched it), so
  `get_plugin_info` reported the wrong bridge version and the runtime self-check logged a false mismatch
  on every startup. Now derives from `env!("CARGO_PKG_VERSION")` so it can never drift.
- **`detect_ghost_commands` output clarified** — the report now has separate `frontend_only` (true ghosts:
  frontend-invoked with no backend handler) and `registry_only` (registered-but-unused) fields, instead of
  dumping both into one misleading `ghost_commands` list.
- **`list_app_dir` no longer hard-errors on a missing dir** — returns a structured `{ exists:false,
  entries:[], count:0 }` (a fresh app with no data dir is queryable state, not a failure). Both
  `list_app_dir` and `read_app_file` now run a lexical traversal guard *before* the existence check (so a
  traversal attempt is rejected as traversal, not reported as "not found").
- **Chrome test dependency CVE** — bumped `vitest` `^3.2.1` → `^4.1.8` (GHSA-5xrq-8626-4rwp, critical);
  `npm audit` is clean and all 169 bridge tests still pass.
- **Native-messaging tests** no longer write binary protocol frames to `cargo test` stdout (the dispatcher's
  writer is now injectable; tests use a sink), so real failures aren't buried in noise.
- **Demo-app accessibility** — primary buttons / tab badges failed Victauri's own WCAG-AA contrast check
  (white on `#8b5cf6` = 4.23:1); darkened to pass; low-contrast muted text raised too.
- **Metadata/doc fixes** — VS Code `package-lock.json` synced to its `package.json` version; npm README no
  longer advertises an unbuilt Linux-aarch64 binary; `examples/demo-app/test_deep.sh` no longer asserts the
  ancient `0.2.1` version (now version-agnostic) and uses the default port 7373.

### Fixed — red-team audit (second pass)

- **Intent resolution ranked the wrong command** — `resolve("increase counter")` returned `get_counter`
  (whose *name* contains "counter") above `increment` (whose *intent* is literally "increase counter"),
  because there was an exact-*name* bonus but no exact-*intent* bonus. Added `SCORE_EXACT_INTENT` so a
  query that exactly matches a command's natural-language intent dominates incidental name-substring
  matches. Regression test added.
- **`assert_semantic` no longer requires `label`** — the field was a required `String`, so a minimal
  `{expression, condition}` call failed deserialization with an opaque 400 (the tool description never
  mentioned it). `label` is now `#[serde(default)]`.
- **`recording.checkpoint` auto-generates an id** — `checkpoint_id` is now optional; when omitted a
  `cp-<uuid>` is generated and echoed back, so a quick positional marker no longer hard-errors with
  "missing checkpoint_id".
- **Stale demo/test scripts** — `examples/demo-app/test_deep.sh` called `audit_accessibility` as a
  standalone tool (it is an `inspect` action); fixed. Removed the long-superseded root `test_live.sh`,
  which predated the compound-tool refactor and called dozens of tool names that no longer exist.

## [0.7.4] - 2026-06-02

### Fixed — agents could bind the WRONG app / wedge on restart (CDP-fallback root cause)

- **`victauri bridge` now guarantees the agent reaches the RIGHT app, dynamically.** A static
  `.mcp.json` `url:` hardcodes a port; when several Victauri apps run (or one falls back off a
  busy 7373) that port can point at a *different* app, so every `mcp__victauri__*` call fails
  with 404/422 and agents give up and use CDP. The bridge now resolves the backend **by app
  identity**: discovery records the Tauri `identifier` + `product_name`, and
  `victauri bridge --app <identifier>` (or `VICTAURI_APP`) binds that exact app regardless of
  which port it landed on. With no selector it uses the single running app, or errors clearly
  listing the running apps — never a silent wrong-app binding.
- **Transparent restart recovery.** Every dev rebuild/relaunch invalidates the MCP session. The
  bridge now caches the `initialize` handshake and re-establishes a fresh session
  (re-discovering the port) on a stale session (404/409/422) or connection drop — the previous
  code cleared the session then replayed the tool call with no session, which the server 422'd.
  Verified by an E2E test that drives the real binary through a simulated restart.
- **First-contact verification.** `/info` and `get_plugin_info` now report `app.identifier`, so
  an agent can confirm it reached the intended app.
- **`victauri init` bakes `--app <identifier>`** into `.mcp.json` (read from `tauri.conf.json`)
  for zero-config multi-app correctness; the generated CLAUDE.md teaches agents to verify
  identity, pin `--app`, and use the sessionless REST API — not CDP — on any wedge.

## [0.7.3] - 2026-06-01

### Security — red-team / audit hardening (release blockers)

- **Browser-extension bridge provenance (audit #2) — re-announce leak closed.** The previous
  nonce gate generated the secret in the MAIN world and re-broadcast it on a perpetual,
  page-triggerable `__victauri_handshake_req` listener. Because the MAIN world shares the page's
  `window`, a hostile page could fire that request, capture the re-announced nonce, and then drive
  the privileged bridge (`eval`, `getCookies`, `getLocalStorage`, …). The nonce is now **generated
  in the ISOLATED relay** (page JS cannot read that scope) and handed to MAIN via a **single-shot
  responder** that is spent by the legitimate `document_start` pull — so a page can never re-elicit
  it. Applies to both Chrome and Firefox extensions. New regression tests reproduce the exfiltration
  attack and assert it fails (Chrome vitest: 169 pass, +6 provenance tests). **Honest scope:** this
  closes the *proactive re-announce* recovery, but the nonce is still observable on a legitimate
  command event (MAIN world == page world) — so the nonce gate remains a hardened *speed-bump*, not a
  true boundary; the capabilities that matter (httpOnly cookies, screenshots) stay service-worker-gated.
  See `docs/src/security.md`.
- **`query_db` / `introspect db_health` no longer select the wrong database (red-team blocker).**
  Selection previously took the first `.db` a directory walk happened to enumerate, with no ranking
  and no exclusion of WebView/browser-engine internal stores — so an agent could confidently inspect
  a WebView profile DB instead of the app's real DB. Now: (a) WebView/engine internals (Cookies,
  QuotaManager, IndexedDB, Local Storage, WebKit/EBWebView dirs, …) are excluded; (b) the largest
  remaining candidate wins; (c) configured `db_search_paths` are **exclusive** when set (no silent
  fallback to OS app dirs); (d) a clear, actionable error is returned when only internals are present
  instead of querying the wrong DB. (4 new tests, incl. the exact red-team layout.)
- **`css inject` blocks remote `@import` / `url(...)` by default (audit / red-team).** Injected CSS
  was added to the page verbatim, so `@import url(https://attacker/…)` or `url(//host)` turned a
  debugging tool into a data-exfiltration / SSRF channel (acute when chained with page-sourced prompt
  injection). A comment-stripping sanitizer now rejects `@import` and remote `url(...)` targets;
  relative, `data:`, and `#fragment` refs are allowed. New `allow_remote: true` param opts back in.
  (4 new tests.)
- **`app_info` env-var secret denylist, dialog fail-closed, command-filter enforcement, core DoS
  bounds** — verified intact at this HEAD (audit #5, #32, #30/#31, #14/#18-21).

### Changed

- **`VictauriClient` auto-recovers from a stale MCP session (HTTP 422).** When a tool call returns
  HTTP 422 "expected initialized request" — the session went stale because the in-app server
  restarted, the client reconnected, or `notifications/initialized` was missed — the client now
  re-runs the handshake once and retries transparently (bounded, no re-init loop). If the session is
  still stale, the error names the cause and points to the sessionless REST endpoint
  (`POST /api/tools/{name}`). Closes #2.
- **`eval_js` fails fast on malformed syntax instead of hanging for the full timeout.** A syntax
  error in the submitted code previously broke the parse of the whole injected wrapper, so the
  callback never fired and the call blocked for the entire (30s) eval timeout. A CSP-safe parse
  watchdog now reports a likely syntax error in ~0.75s. (We intentionally do **not** use
  `new Function`/`AsyncFunction` to surface the `SyntaxError`, because dynamic code generation is
  gated by the same `unsafe-eval` CSP that blocks `eval()` — which is exactly why the bridge uses an
  inline async-IIFE. The watchdog distinguishes a parse failure, which never marks the script
  "started", from valid-but-slow code, which is left to run to the real timeout.)
- **`app_info.databases` now returns objects, not bare strings.** Each entry is
  `{ path, size_bytes, webview_internal, selected }` across **all** roots (configured
  `db_search_paths` + every OS app dir), so an agent can see and disambiguate the real app DB and
  which one `query_db` would auto-select. (Previously: relative path strings from `data_dir` only.)
- **Auth documentation reconciled with auth-on-by-default.** 12 locations across the mdbook docs,
  READMEs, `SECURITY.md`, and a stale crate doc-comment/test that still said auth was "off/optional
  by default" now correctly describe auth as enabled by default (auto-generated, auto-discovered
  token) with `.auth_disabled()` to opt out.

### Fixed

- **Dev-dependency advisories** — `ws` (bridge-test harness, GHSA-58qx-3vcg-4xpx, moderate) bumped to
  8.21.0; `tmp` (VS Code `@vscode/vsce` packaging tooling, GHSA-ph9p-34f9-6g65, high) bumped to 0.2.7.
  Both dev/build-only (neither ships to end users); non-breaking lockfile updates.
- **E2E regression harnesses** — `scripts/e2e/01,02` updated from the stale `0.5.6` / 31-tool
  assertions to `0.7.2` / 34 tools, and the tool-registration loop now includes `route`/`trace`/
  `animation`.

## [0.7.2] - 2026-05-31

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

### Changed — Loud failure on blank window capture

- **Native window capture no longer returns a silent blank frame on transparent/composited windows.** GDI capture (`PrintWindow`/`BitBlt`) cannot see a transparent or GPU-composited window (no DWM redirection surface) — it previously returned an all-white/empty image that looked like a successful capture, so `animation scrub`'s filmstrip would silently produce a blank. `capture_window_raw` now detects the uniform blank frame (and reports whether `WS_EX_LAYERED`/`WS_EX_NOREDIRECTIONBITMAP` is set) and **fails with an actionable error naming the OS desktop-composite workaround** instead. Opaque-window capture is unchanged. (5 new unit tests.)

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

- **BREAKING:** **victauri-plugin**: Authentication **disabled by default** — the MCP server binds to `127.0.0.1` only and the plugin is `#[cfg(debug_assertions)]`-gated, so auth adds friction without meaningful security for local dev. Use `auth_enabled()`, `auth_token("...")`, or `VICTAURI_AUTH_TOKEN` env var to opt in. **(Historical: REVERSED in [0.5.6] — auth has been ON by default since v0.5.6 with an auto-generated token; opt out with `auth_disabled()`. This 0.4.0 default is no longer current.)**
- **victauri-plugin**: `auth_disabled()` is now a backwards-compatible no-op (auth is already off by default) **(Historical — no longer true since [0.5.6]: `auth_disabled()` now actually disables the default auto-token.)**
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

[Unreleased]: https://github.com/4DA-Systems/victauri/compare/v0.8.5...HEAD
[0.8.5]: https://github.com/4DA-Systems/victauri/compare/v0.8.4...v0.8.5
[0.8.4]: https://github.com/4DA-Systems/victauri/compare/v0.8.3...v0.8.4
[0.8.3]: https://github.com/4DA-Systems/victauri/compare/v0.8.2...v0.8.3
[0.8.2]: https://github.com/4DA-Systems/victauri/compare/v0.8.1...v0.8.2
[0.8.1]: https://github.com/4DA-Systems/victauri/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/4DA-Systems/victauri/compare/v0.7.11...v0.8.0
[0.5.3]: https://github.com/4DA-Systems/victauri/compare/v0.5.2...v0.5.3
[0.5.2]: https://github.com/4DA-Systems/victauri/compare/v0.5.0...v0.5.2
[0.5.0]: https://github.com/4DA-Systems/victauri/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/4DA-Systems/victauri/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/4DA-Systems/victauri/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/4DA-Systems/victauri/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/4DA-Systems/victauri/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/4DA-Systems/victauri/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/4DA-Systems/victauri/releases/tag/v0.1.0
