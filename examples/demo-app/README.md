# Victauri Demo App

Multi-window Tauri 2 app instrumented with Victauri. Demonstrates full-stack testing patterns including CRUD, form validation, navigation, notifications, and cross-boundary state verification.

All 19 commands use `#[inspectable]` and are registered in the Victauri command registry, making them discoverable via MCP.

## Commands

| Category | Command | Description |
|----------|---------|-------------|
| general | `greet` | Returns a greeting for the given name |
| counter | `get_counter` | Reads the current counter value |
| counter | `increment` | Increments the counter by 1 |
| counter | `decrement` | Decrements the counter by 1 |
| counter | `reset_counter` | Resets the counter to zero |
| todos | `add_todo` | Creates a new todo item |
| todos | `list_todos` | Lists all todo items |
| todos | `toggle_todo` | Toggles a todo's completion status |
| todos | `delete_todo` | Deletes a todo by ID |
| settings | `get_settings` | Returns current app settings |
| settings | `update_settings` | Updates theme, notifications, or language |
| contacts | `submit_contact` | Submits contact form with server-side validation |
| contacts | `list_contacts` | Lists all submitted contacts |
| notifications | `send_notification` | Creates a notification (emits event to all windows) |
| notifications | `list_notifications` | Lists all notifications |
| notifications | `mark_notification_read` | Marks a notification as read |
| notifications | `unread_count` | Returns count of unread notifications |
| windows | `show_notification_window` | Opens or focuses the notification panel window |
| debug | `get_app_state` | Dumps full application state as JSON |

## Features Demonstrated

- **Tab navigation** with proper ARIA attributes
- **CRUD operations** (todos with create, read, toggle, delete)
- **Form validation** (contact form with server-side field-level errors)
- **Multi-window** (main + notification panel with event sync)
- **Settings persistence** (theme, language, notifications toggle)
- **Cross-window events** (`notification-added` event broadcast)
- **Accessibility** (ARIA roles, labels, live regions throughout)
- **`data-testid` attributes** on all interactive elements for test targeting

## Run

```bash
cd examples/demo-app
cargo tauri dev
```

## Test

With the app running:
```bash
VICTAURI_E2E=1 cargo test -p demo-app --test integration
```

The integration tests demonstrate every Victauri testing pattern:
- Direct client API (`click_by_id`, `fill_by_id`, `eval_js`)
- Locator API (`Locator::text()`, `Locator::test_id()`, expectations)
- IPC verification (`invoke_command`, `verify().ipc_was_called()`)
- Cross-boundary state checks (`verify().state_matches()`)
- Accessibility auditing (`audit_accessibility`)
- Performance monitoring (`get_performance_metrics`)
- Time-travel recording (`start_recording`, `checkpoint`, `stop_recording`)
- Fluent assertions (`verify().has_text().no_console_errors().run()`)
- Built-in smoke test suite

## MCP Configuration

The included `.mcp.json` allows Claude Code to connect immediately:

```json
{
  "mcpServers": {
    "victauri-demo": {
      "url": "http://127.0.0.1:7373/mcp"
    }
  }
}
```
