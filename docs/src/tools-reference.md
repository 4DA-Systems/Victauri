# Tools Reference

Victauri exposes 31 MCP tools organized into standalone tools (one action per call) and compound tools (multiple actions via an `action` parameter).

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
| `dir_type` | string | no | One of: `data`, `config`, `log`, `local_data` (default: `data`) |
| `subpath` | string | no | Subdirectory to list within the chosen directory |

**Returns:** `{path, entries: [{name, size, is_dir, modified}]}`

---

### read_app_file

Read a file from one of the app's backend directories.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `path` | string | yes | File path relative to the directory root |
| `dir_type` | string | no | One of: `data`, `config`, `log`, `local_data` (default: `data`) |

**Returns:** `{content, encoding, size}` — UTF-8 text or base64-encoded binary.

---

### query_db

Execute a read-only SQL query against a SQLite database in the app's data directory.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `sql` | string | yes | SQL query (SELECT only) |
| `db_path` | string | no | Path to database file (auto-discovers if omitted) |
| `params` | array | no | Bind parameters for the query |

**Examples:**
```json
{"sql": "SELECT * FROM users WHERE active = ?", "params": [true]}
{"sql": "SELECT count(*) FROM items", "db_path": "app.db"}
```

**Returns:** `{columns, rows, row_count}`

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

Bare expressions are auto-wrapped with `return`. Multi-statement code and async/await are supported.

---

### dom_snapshot

Capture a full accessible DOM tree with ref handles for every element.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `webview_label` | string | no | Target webview |

**Returns:** Tree of elements with `ref`, `role`, `name`, `children`, and bounding box data.

---

### find_elements

Search for elements by CSS selector or text content.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `selector` | string | no | CSS selector (alias: `css`) |
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
| `condition` | string | yes | One of: `selector`, `selector_gone`, `text`, `text_gone`, `url` |
| `value` | string | yes | The selector, text, or URL pattern to match |
| `timeout_ms` | number | no | Max wait time in ms (default: 5000) |

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

Element interactions with actionability checks.

| Action | Parameters | Description |
|--------|-----------|-------------|
| `click` | `ref_id` | Click an element |
| `hover` | `ref_id` | Hover over an element |
| `focus` | `ref_id` | Focus an element |
| `scroll_into_view` | `ref_id` | Scroll element into viewport |
| `select` | `ref_id`, `value` | Select an option |

**Example:**
```json
{"action": "click", "ref_id": "e3"}
{"action": "hover", "ref_id": "e12"}
```

---

### input

Text input and keyboard operations.

| Action | Parameters | Description |
|--------|-----------|-------------|
| `fill` | `ref_id`, `value` | Set input value directly |
| `type` | `ref_id`, `text` | Type character-by-character |
| `press_key` | `key` | Press a keyboard key |

**Example:**
```json
{"action": "fill", "ref_id": "e5", "value": "hello@example.com"}
{"action": "type", "ref_id": "e5", "text": "Hello"}
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
| `checkpoint` | `label` | Create a named checkpoint |
| `events` | `since`, `limit` | Get recorded events |
| `export` | — | Export full session data |
| `import` | `session` | Import a session for replay |
| `replay` | `webview_label` | Re-execute recorded IPC commands and report pass/fail per command |

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

Inject faults into Tauri IPC commands at the Rust layer for chaos engineering. CDP cannot inject failures at the backend.

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
