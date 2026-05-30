# Tools Reference

Victauri exposes 33 MCP tools organized into standalone tools (one action per call) and compound tools (multiple actions via an `action` parameter).

All tools are accessible via MCP at `/mcp` or REST at `POST /api/tools/{tool_name}`.

## Backend Tools

These tools access the Rust backend directly — no webview proxy, no JavaScript evaluation.

### app_info

Get application configuration, directory paths, environment, discovered databases, and process info.

**Parameters:** None required.

**Returns:** `{config, paths, databases, process, environment}`

---

### list_app_dir

Browse files in app backend directories (data, config, log, local_data).

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `directory` | string | no | One of: `data`, `config`, `log`, `local_data` (default: `data`) |
| `path` | string | no | Subdirectory to list within the chosen directory |
| `pattern` | string | no | Glob to filter entries (e.g. `*.db`) |
| `max_depth` | number | no | Recursion depth (default: 1) |

**Returns:** `{root, entries: [{name, path, is_dir, size, modified}]}`

---

### read_app_file

Read a file from one of the app's backend directories.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `path` | string | yes | File path relative to the directory root |
| `directory` | string | no | One of: `data`, `config`, `log`, `local_data` (default: `data`) |
| `max_bytes` | number | no | Max bytes to read (default: 1 MB) |
| `binary` | boolean | no | Return base64 instead of UTF-8 text |

**Returns:** UTF-8 text, or base64-encoded bytes when `binary` is true. Path-traversal-guarded.

---

### query_db

Execute a read-only SQL query against a SQLite database in the app's data directory.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `query` | string | yes | SQL query — `SELECT`/`PRAGMA`(read)/`EXPLAIN`/`WITH` only |
| `path` | string | no | Path to the database file (auto-discovers if omitted) |
| `params` | array | no | Positional bind parameters |
| `max_rows` | number | no | Max rows to return (default 100) |

**Examples:**
```json
{"query": "SELECT * FROM users WHERE active = ?", "params": [true]}
{"query": "SELECT count(*) FROM items", "path": "app.db"}
```

**Returns:** `{columns, rows, row_count, truncated, database}`

Read-only and path-traversal-guarded: writes (`INSERT`/`UPDATE`/…), stacked
queries, `ATTACH`, and the write form of `PRAGMA` (`PRAGMA x = y`) are rejected.
By default only the OS app-data directories are searched; if your app stores its
DB elsewhere (a project/working dir or custom path), register the directory via
`VictauriBuilder::db_search_paths(["../data", "/abs/path"])` — then relative names
and absolute paths within those roots become reachable.

---

## Webview & IPC Tools

### eval_js

Evaluate JavaScript in the webview and return the result.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `code` | string | yes | JavaScript code to evaluate (expressions, statements, or async/await) |
| `webview_label` | string | no | Target webview (defaults to "main" or first visible) |

**Examples:**
```json
{"code": "document.title"}
{"code": "document.querySelectorAll('button').length"}
{"code": "await fetch('/api/data').then(r => r.json())"}
```

**Auto-`return`:** a single bare expression is auto-wrapped with `return`
(`document.title` → `return document.title`). **Multi-statement code must include
an explicit `return`** — e.g. `localStorage.setItem('k','v'); return localStorage.getItem('k')` —
or be wrapped in an IIFE; otherwise only the first statement runs. async/await is
supported.

JavaScript errors (thrown exceptions) return an MCP error with `isError: true`.
`undefined` returns `"undefined"`, `null` returns `null`. A **syntax error**
surfaces only as the eval timeout (the webview cannot report parse errors to the
host). Targeting a hidden or unresponsive window fails fast (~2s); and if a prior
eval timed out, the next call re-probes and fails fast if the webview reloaded or
the app stopped responding.

---

### dom_snapshot

Capture a full accessible DOM tree with ref handles for every element.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `webview_label` | string | no | Target webview |

**Returns:** Tree of elements with `ref`, `role`, `name`, `children`, and bounding box data. Descends into open shadow DOM and **same-origin iframes** (cross-origin frames are marked and skipped).

---

### find_elements

Search for elements by CSS selector or text content. Returns an MCP error for invalid CSS selectors. Searches into open shadow roots and **same-origin iframes** — frame elements get ref handles and are fully interactable.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `selector` | string | no | CSS selector (alias: `css`). Invalid selectors return an error. |
| `text` | string | no | Text content to search for |
| `role` | string | no | ARIA role to filter by |
| `webview_label` | string | no | Target webview |

**Examples:**
```json
{"selector": "button.primary"}
{"text": "Submit"}
{"role": "heading"}
```

---

### invoke_command

Invoke a Tauri command from the backend.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `command` | string | yes | Command name |
| `args` | object | no | Arguments to pass |

**Example:**
```json
{"command": "get_settings", "args": {}}
{"command": "search_context", "args": {"query": "hello"}}
```

---

### screenshot

Capture a PNG screenshot of the application window.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `window_label` | string | no | Target window (defaults to main) |

**Returns:** Base64-encoded PNG image data.

---

### verify_state

Compare frontend and backend state to detect drift.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `frontend_expr` | string | no | JS expression for frontend state |
| `backend_state` | object | no | Expected backend state to compare |

**Example:**
```json
{
  "frontend_expr": "document.title",
  "backend_state": {"title": "My App"}
}
```

---

### detect_ghost_commands

Find commands invoked by the frontend that are not registered in the backend registry.

**Parameters:** None required.

**Returns:** List of ghost commands with invocation counts.

---

### check_ipc_integrity

Verify the health of IPC communication.

**Parameters:** None required.

**Returns:** `{healthy, total_calls, pending, stale, errored}`

---

### wait_for

Wait for a condition to become true, polling until timeout.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `condition` | string | yes | One of: `selector`, `selector_gone`, `text`, `text_gone`, `url`, `ipc_idle`, `network_idle` |
| `value` | string | no | Selector/text/URL to match (not needed for `ipc_idle`/`network_idle`) |
| `timeout_ms` | number | no | Max wait time in ms (default: 10000) |
| `poll_ms` | number | no | Poll interval in ms (default: 200) |

**Example:**
```json
{"condition": "selector", "value": ".modal.open", "timeout_ms": 3000}
{"condition": "url", "value": "/dashboard"}
```

---

### assert_semantic

Assert a condition about the application state using JS expressions.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `expression` | string | yes | JS expression to evaluate |
| `condition` | string | yes | One of: `equals`, `not_equals`, `contains`, `greater_than`, `less_than`, `truthy`, `falsy` |
| `expected` | any | no | Expected value (not needed for truthy/falsy) |

**Example:**
```json
{
  "expression": "document.title",
  "condition": "equals",
  "expected": "My App"
}
```

---

### resolve_command

Resolve a natural language description to registered commands.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `query` | string | yes | Natural language description |

**Example:**
```json
{"query": "show settings"}
```

---

### get_registry

List all registered commands with their metadata.

**Parameters:** None.

---

### get_memory_stats

Get real OS process memory usage.

**Parameters:** None.

**Returns:** `{working_set_bytes, peak_working_set_bytes, page_fault_count, page_file_bytes}`

---

### get_plugin_info

Get plugin version, uptime, configuration, and capabilities.

**Parameters:** None.

---

### get_diagnostics

Get detailed diagnostic information about the plugin state.

**Parameters:** None.

---

## Compound Tools

Compound tools use an `action` parameter to select the specific operation.

### interact

Element interactions with Playwright-grade actionability checks. Works on
elements inside same-origin iframes too (refs from a snapshot/find resolve
across frame boundaries).

| Action | Parameters | Description |
|--------|-----------|-------------|
| `click` | `ref_id` | Click an element |
| `double_click` | `ref_id` | Double-click an element |
| `hover` | `ref_id` | Hover over an element |
| `focus` | `ref_id` | Focus an element |
| `scroll_into_view` | `ref_id` | Scroll element into viewport |
| `select_option` | `ref_id`, `value` or `values` | Select option(s) in a `<select>` |

**Trusted (OS-level) clicks:** add `"trusted": true` to `click` to deliver a
real OS mouse event (`isTrusted: true`) at the element's center, instead of a
synthetic DOM event — for app handlers that gate on `event.isTrusted` or browser
features needing user activation. Implemented on Windows (Win32 `SendInput`);
on macOS/Linux it returns a clear "not implemented" error so you fall back to the
default synthetic click. The window is brought to the foreground first.

**Example:**
```json
{"action": "click", "ref_id": "e3"}
{"action": "click", "ref_id": "e3", "trusted": true}
{"action": "select_option", "ref_id": "e9", "value": "us"}
```

---

### input

Text input and keyboard operations.

| Action | Parameters | Description |
|--------|-----------|-------------|
| `fill` | `ref_id`, `value` | Set input value directly |
| `type` | `ref_id`, `text` | Type character-by-character |
| `press_key` | `key` | Press a keyboard key |

**Trusted (OS-level) input:** add `"trusted": true` to `type` or `press_key` to
deliver real OS keystrokes (`isTrusted: true`) into the focused element instead
of synthetic DOM events. The target element (`ref_id`) is focused first.
Implemented on Windows (Win32 `SendInput`); macOS/Linux fall back to synthetic
input with a clear error.

**Example:**
```json
{"action": "fill", "ref_id": "e5", "value": "hello@example.com"}
{"action": "type", "ref_id": "e5", "text": "Hello"}
{"action": "type", "ref_id": "e5", "text": "Hello", "trusted": true}
{"action": "press_key", "key": "Enter"}
```

Supported keys: `Tab`, `Escape`, `Enter`, `ArrowUp`, `ArrowDown`, `ArrowLeft`, `ArrowRight`, `F1`-`F12`, and any single character.

---

### window

Window management operations.

| Action | Parameters | Description |
|--------|-----------|-------------|
| `get_state` | `label` | Get window state (position, size, visibility) |
| `list` | — | List all window labels |
| `manage` | `label`, `operation` | minimize/unminimize/maximize/unmaximize/close |
| `resize` | `label`, `width`, `height` | Resize a window |
| `move_to` | `label`, `x`, `y` | Move a window |
| `set_title` | `label`, `title` | Change window title |

**Example:**
```json
{"action": "list"}
{"action": "get_state", "label": "main"}
{"action": "resize", "label": "main", "width": 1200, "height": 800}
```

---

### storage

Browser storage operations.

| Action | Parameters | Description |
|--------|-----------|-------------|
| `get` | `key` | Get localStorage value |
| `set` | `key`, `value` | Set localStorage value |
| `delete` | `key` | Delete localStorage key |
| `cookies` | — | Get all cookies |

**Example:**
```json
{"action": "set", "key": "theme", "value": "dark"}
{"action": "get", "key": "theme"}
```

---

### navigate

Navigation and history operations.

| Action | Parameters | Description |
|--------|-----------|-------------|
| `go_to` | `url` | Navigate to a URL (http/https only) |
| `back` | — | Go back in history |
| `history` | — | Get navigation history log |
| `dialogs` | — | Get dialog log (alerts, confirms, prompts) |

**Example:**
```json
{"action": "go_to", "url": "https://example.com"}
{"action": "history"}
```

---

### recording

Time-travel recording for session capture and replay.

| Action | Parameters | Description |
|--------|-----------|-------------|
| `start` | — | Start recording events |
| `stop` | — | Stop recording and return session |
| `checkpoint` | `label` (alias: `checkpoint_label`) | Create a named checkpoint |
| `events` | `since`, `limit` | Get recorded events |
| `export` | — | Export full session data (works after stop) |
| `import` | `session` | Import a session for replay |
| `replay` | `webview_label` | Re-execute recorded IPC commands and report pass/fail per command (works after stop) |

**Example:**
```json
{"action": "start"}
{"action": "checkpoint", "label": "after-login"}
{"action": "stop"}
{"action": "replay"}
```

---

### inspect

CSS inspection, accessibility, and performance profiling.

| Action | Parameters | Description |
|--------|-----------|-------------|
| `styles` | `ref_id`, `properties` | Get computed CSS styles |
| `bounds` | `ref_ids` | Get bounding boxes with box model |
| `highlight` | `ref_id`, `color`, `label` | Draw debug overlay on element |
| `accessibility` | — | Run WCAG accessibility audit |
| `performance` | — | Get performance metrics |

**Example:**
```json
{"action": "styles", "ref_id": "e3", "properties": ["color", "font-size"]}
{"action": "bounds", "ref_ids": ["e1", "e2", "e3"]}
{"action": "accessibility"}
{"action": "performance"}
```

The accessibility audit checks: missing alt text, unlabeled form inputs, empty buttons/links, heading hierarchy, color contrast (WCAG AA), ARIA role validity, positive tabindex, and missing document language/title.

Performance metrics include: navigation timing, resource summary, paint timing (FP/FCP), JS heap usage, long task count, and DOM statistics.

---

### css

CSS injection for debugging and prototyping.

| Action | Parameters | Description |
|--------|-----------|-------------|
| `inject` | `css` | Inject custom CSS (replaces previous) |
| `remove` | — | Remove injected CSS |

**Example:**
```json
{"action": "inject", "css": "* { outline: 1px solid red; }"}
{"action": "remove"}
```

---

### logs

Access all captured logs from the application.

| Action | Parameters | Description |
|--------|-----------|-------------|
| `console` | `since`, `level` | Console log entries |
| `network` | `since` | Network request log |
| `ipc` | `since`, `limit` | IPC command log |
| `navigation` | — | Navigation history |
| `dialogs` | — | Dialog interactions |
| `events` | `since` | Event stream |
| `slow_ipc` | `threshold_ms` | IPC calls slower than threshold |

**Example:**
```json
{"action": "console", "level": "error"}
{"action": "network"}
{"action": "slow_ipc", "threshold_ms": 100}
```

> `ipc`/`network`/`slow_ipc` return at most 100 entries by default and truncate
> large per-entry bodies (4 KB) — pass an explicit `limit` for more.

---

### route

Network request interception — the Playwright `route()` equivalent, implemented
purely in the JS bridge (no CDP, works identically across all Tauri webviews).
Matches webview `fetch`/XHR by URL and blocks, mocks, or delays them. Rules are
page-scoped (cleared on reload).

| Action | Parameters | Description |
|--------|-----------|-------------|
| `add` | `pattern`, `behavior`, … | Add a rule (see below) |
| `list` | — | List active rules |
| `clear` | `id` | Remove a rule by id |
| `clear_all` | — | Remove all rules |
| `matches` | `limit` | Log of intercepted requests |

**`add` parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `pattern` | string | yes | URL pattern to match |
| `match_type` | string | no | `substring` (default), `glob`, `regex`, `exact` |
| `method` | string | no | Restrict to one HTTP method |
| `behavior` | string | no | `block` (abort), `fulfill` (mock — default), `delay` |
| `status` | number | no | Mock response status (fulfill, default 200) |
| `headers` | object | no | Mock response headers (fulfill) |
| `body` | any | no | Mock response body (fulfill) |
| `content_type` | string | no | Mock content-type (fulfill, default `application/json`) |
| `delay_ms` | number | no | Latency to inject (delay; also delays a fulfill) |
| `times` | number | no | Max times the rule fires (0 = unlimited) |

**Example:**
```json
{"action": "add", "pattern": "/api/users", "behavior": "fulfill", "status": 200, "body": {"users": []}}
{"action": "add", "pattern": "analytics", "behavior": "block"}
{"action": "add", "pattern": "*/slow", "match_type": "glob", "behavior": "delay", "delay_ms": 2000}
{"action": "matches"}
```

> **Scope:** fetch supports all behaviors; XHR supports `block`/`delay`
> (`fulfill` is fetch-only). Top-level navigation, sub-resources (img/css), and
> WebSocket frames are not intercepted. For Tauri **IPC-layer** faults, use the
> `fault` tool instead.

---

### trace

Screencast / visual timeline — captures the window at a fixed interval into a
ring buffer via the native screenshot path (no CDP). Pairs with `recording`
(events) and `logs` (network/console) to form a Playwright-trace-style bundle.

| Action | Parameters | Description |
|--------|-----------|-------------|
| `start` | `interval_ms`, `max_frames`, `with_events` | Begin capturing |
| `stop` | — | Stop and return a summary |
| `status` | — | Active flag + buffered frame count |
| `frames` | `limit` | Return captured frames as base64 PNGs |

`start` defaults: `interval_ms` 500 (min 50), `max_frames` 60 (max 600). Set
`with_events: true` to also start the event recorder so the trace bundles the
IPC/DOM/console timeline alongside the screencast.

**Example:**
```json
{"action": "start", "interval_ms": 250, "max_frames": 40, "with_events": true}
{"action": "status"}
{"action": "stop"}
{"action": "frames", "limit": 10}
```

---

## Backend Introspection (Victauri-Exclusive)

These tools exploit Victauri's position inside the Rust process to provide insights and control that browser-external tools like CDP cannot access.

### introspect

Deep backend introspection — command performance profiling, IPC contract testing, coverage analysis, startup timing, capability auditing, process enumeration, and event bus monitoring.

| Action | Parameters | Description |
|--------|-----------|-------------|
| `command_timings` | `slow_threshold_ms` | Per-command execution timing stats (min/max/avg/p95) |
| `coverage` | — | Which registered commands have been called this session |
| `contract_record` | `command`, `args` | Record a command's response shape as baseline |
| `contract_check` | — | Check all recorded contracts for schema drift |
| `contract_list` | — | List all recorded contract baselines |
| `contract_clear` | — | Clear all recorded contract baselines |
| `startup_timing` | — | Victauri plugin initialization phase-by-phase timing breakdown |
| `capabilities` | — | Tauri v2 capabilities, security config (CSP, freeze_prototype), plugins, and window definitions |
| `db_health` | `db_path` | `SQLite` database diagnostics (journal mode, WAL, page stats) |
| `plugin_state` | — | Victauri plugin internal state: event counts, registry, recording, faults, timings, uptime |
| `processes` | — | Host process + child processes (sidecars, background workers) with PID, name, and memory |
| `plugin_tasks` | — | Victauri's spawned async tasks (MCP server, event drain) with active/finished counts |
| `event_bus` | — | All captured Tauri events (automatically intercepted) + app events from EventLog |
| `event_bus_clear` | — | Clear both event bus and event log |

**Examples:**
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

---

### fault

Probe a backend command handler under failure for chaos engineering.

> **Scope:** faults apply **only** to commands you run via the `invoke_command` tool — they do **not** intercept the app's real user-driven IPC (`window.__TAURI_INTERNALS__.invoke`), which Tauri serves below the JS layer Victauri can reach. Use `fault` to test a handler's error path when *you* drive it (e.g. "does my error branch return the right shape on a DB failure?"). It does not reproduce a failure a user clicking the UI would experience — that path is not interceptable cross-platform without CDP.

| Action | Parameters | Description |
|--------|-----------|-------------|
| `inject` | `command`, `fault_type`, `delay_ms`, `error_message`, `max_triggers` | Add a fault rule |
| `list` | — | List all active fault injection rules |
| `clear` | `command` | Remove a specific fault rule |
| `clear_all` | — | Remove all fault rules |

**Fault types:** `delay` (add latency), `error` (return error), `drop` (empty response), `corrupt` (mangle response).

**Examples:**
```json
{"action": "inject", "command": "get_settings", "fault_type": "delay", "delay_ms": 2000}
{"action": "inject", "command": "save_data", "fault_type": "error", "error_message": "disk full"}
{"action": "inject", "command": "fetch_feed", "fault_type": "drop", "max_triggers": 3}
{"action": "list"}
{"action": "clear_all"}
```

---

### explain

Natural-language narration of what happened in the app. Aggregates events from the EventLog over a time window and produces human-readable summaries, causal chains, or diffs.

| Action | Parameters | Description |
|--------|-----------|-------------|
| `summary` | `seconds` | Aggregate events over N seconds (default: 30) into a narrative with type counts |
| `last_action` | `seconds` | Map recent events to a causal chain with " → " separators (default: 5s) |
| `diff` | `seconds` | Count IPC calls, DOM changes, errors, and interactions over N seconds (default: 10) |

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `action` | string | yes | One of: `summary`, `last_action`, `diff` |
| `seconds` | integer | no | How many seconds to look back |
| `webview_label` | string | no | Target webview window |

**Examples:**
```json
{"action": "summary"}
{"action": "summary", "seconds": 60}
{"action": "last_action"}
{"action": "diff", "seconds": 15}
```
