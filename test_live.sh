#!/bin/bash
set -e

BASE="http://127.0.0.1:7373"
PASS=0
FAIL=0
ERRORS=""

# Initialize MCP session
SESSION_RESP=$(curl -s -X POST "$BASE/mcp" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"0.1.0"}}}' \
  -D /tmp/mcp_headers.txt 2>/dev/null)

MCP_SESSION=$(grep -i 'mcp-session-id' /tmp/mcp_headers.txt | tr -d '\r' | awk '{print $2}')
echo "Session: $MCP_SESSION"

# Send initialized notification
curl -s -X POST "$BASE/mcp" \
  -H "Content-Type: application/json" \
  -H "mcp-session-id: $MCP_SESSION" \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized"}' > /dev/null 2>&1

call_tool() {
  local name="$1"
  local args="$2"
  local id=$((RANDOM % 9000 + 1000))
  local result
  result=$(curl -s -X POST "$BASE/mcp" \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    -H "mcp-session-id: $MCP_SESSION" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/call\",\"params\":{\"name\":\"$name\",\"arguments\":$args}}")
  echo "$result"
}

check() {
  local label="$1"
  local result="$2"
  local check_for="$3"

  if echo "$result" | grep -q "$check_for"; then
    PASS=$((PASS+1))
    echo "  PASS: $label"
  else
    FAIL=$((FAIL+1))
    ERRORS="$ERRORS\n  FAIL: $label"
    echo "  FAIL: $label"
    echo "    Got: $(echo "$result" | head -c 300)"
  fi
}

echo ""
echo "=== TOOL COUNT ==="
TOOLS=$(curl -s -X POST "$BASE/mcp" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "mcp-session-id: $MCP_SESSION" \
  -d '{"jsonrpc":"2.0","id":99,"method":"tools/list","params":{}}')
TOOL_COUNT=$(echo "$TOOLS" | grep -o '"name"' | wc -l)
echo "  Tools registered: $TOOL_COUNT"
if [ "$TOOL_COUNT" -ge 55 ]; then
  PASS=$((PASS+1))
  echo "  PASS: tool count >= 55"
else
  FAIL=$((FAIL+1))
  echo "  FAIL: tool count (got $TOOL_COUNT, expected >= 55)"
fi

# Check all new tools appear (Phase 7 + Phase 8)
for t in double_click hover select_option scroll_to focus_element get_network_log get_storage set_storage delete_storage get_cookies get_navigation_log navigate navigate_back get_dialog_log set_dialog_response wait_for manage_window resize_window move_window set_window_title get_styles get_bounding_boxes highlight_element clear_highlights inject_css remove_injected_css audit_accessibility get_performance_metrics; do
  if echo "$TOOLS" | grep -q "\"$t\""; then
    PASS=$((PASS+1))
    echo "  PASS: tool registered: $t"
  else
    FAIL=$((FAIL+1))
    ERRORS="$ERRORS\n  FAIL: tool missing: $t"
    echo "  FAIL: tool missing: $t"
  fi
done

echo ""
echo "=== WEBVIEW TOOLS ==="

R=$(call_tool "eval_js" '{"code":"document.title"}')
check "eval_js (document.title)" "$R" "title"

R=$(call_tool "eval_js" '{"code":"window.__VICTAURI__.version"}')
check "eval_js (bridge v0.2.0)" "$R" "0.2.0"

R=$(call_tool "dom_snapshot" '{}')
check "dom_snapshot" "$R" "ref_id"

R=$(call_tool "click" '{"ref_id":"e3"}')
check "click" "$R" "ok"

R=$(call_tool "double_click" '{"ref_id":"e3"}')
check "double_click" "$R" "ok"

R=$(call_tool "hover" '{"ref_id":"e3"}')
check "hover" "$R" "ok"

R=$(call_tool "type_text" '{"ref_id":"e3","text":"hi"}')
check "type_text" "$R" "ok"

R=$(call_tool "press_key" '{"key":"Escape"}')
check "press_key" "$R" "ok"

R=$(call_tool "scroll_to" '{"x":0,"y":0}')
check "scroll_to (coords)" "$R" "ok"

R=$(call_tool "scroll_to" '{"ref_id":"e3"}')
check "scroll_to (ref)" "$R" "ok"

R=$(call_tool "focus_element" '{"ref_id":"e3"}')
check "focus_element" "$R" "ok"

echo ""
echo "=== WINDOW TOOLS ==="

R=$(call_tool "list_windows" '{}')
check "list_windows" "$R" "main"

R=$(call_tool "get_window_state" '{}')
check "get_window_state" "$R" "visible"

R=$(call_tool "screenshot" '{}')
check "screenshot" "$R" "image"

R=$(call_tool "manage_window" '{"action":"focus"}')
check "manage_window (focus)" "$R" "executed"

R=$(call_tool "set_window_title" '{"title":"Victauri Test"}')
check "set_window_title" "$R" "ok"

R=$(call_tool "set_window_title" '{"title":"TestApp"}')
check "set_window_title (restore)" "$R" "ok"

echo ""
echo "=== IPC PIPELINE (CRITICAL FIX) ==="

R=$(call_tool "invoke_command" '{"command":"get_settings"}')
check "invoke_command (get_settings)" "$R" "content"

R=$(call_tool "invoke_command" '{"command":"get_monitoring_status"}')
check "invoke_command (get_monitoring_status)" "$R" "content"

sleep 1

R=$(call_tool "get_ipc_log" '{}')
check "get_ipc_log (has entries)" "$R" "command"

R=$(call_tool "get_ipc_log" '{"limit":1}')
check "get_ipc_log (limit)" "$R" "command"

R=$(call_tool "detect_ghost_commands" '{}')
check "detect_ghost_commands" "$R" "ghost_commands"

R=$(call_tool "check_ipc_integrity" '{}')
check "check_ipc_integrity" "$R" "healthy"

echo ""
echo "=== NETWORK MONITORING ==="

R=$(call_tool "get_network_log" '{}')
check "get_network_log" "$R" "content"

R=$(call_tool "get_network_log" '{"filter":"localhost","limit":5}')
check "get_network_log (filtered)" "$R" "content"

echo ""
echo "=== STORAGE ==="

R=$(call_tool "set_storage" '{"storage_type":"local","key":"victauri_test","value":"hello_world"}')
check "set_storage (local)" "$R" "ok"

R=$(call_tool "get_storage" '{"storage_type":"local","key":"victauri_test"}')
check "get_storage (local, key)" "$R" "hello_world"

R=$(call_tool "get_storage" '{"storage_type":"local"}')
check "get_storage (local, all)" "$R" "victauri_test"

R=$(call_tool "delete_storage" '{"storage_type":"local","key":"victauri_test"}')
check "delete_storage (local)" "$R" "ok"

R=$(call_tool "get_storage" '{"storage_type":"session"}')
check "get_storage (session)" "$R" "content"

R=$(call_tool "get_cookies" '{}')
check "get_cookies" "$R" "content"

echo ""
echo "=== NAVIGATION ==="

R=$(call_tool "get_navigation_log" '{}')
check "get_navigation_log" "$R" "initial"

R=$(call_tool "navigate_back" '{}')
check "navigate_back" "$R" "ok"

echo ""
echo "=== DIALOGS ==="

R=$(call_tool "get_dialog_log" '{}')
check "get_dialog_log" "$R" "content"

R=$(call_tool "set_dialog_response" '{"dialog_type":"confirm","action":"dismiss"}')
check "set_dialog_response (dismiss)" "$R" "ok"

R=$(call_tool "set_dialog_response" '{"dialog_type":"confirm","action":"accept"}')
check "set_dialog_response (accept)" "$R" "ok"

echo ""
echo "=== CONSOLE ==="

R=$(call_tool "get_console_logs" '{}')
check "get_console_logs" "$R" "content"

echo ""
echo "=== BACKEND ==="

R=$(call_tool "get_registry" '{}')
check "get_registry" "$R" "content"

R=$(call_tool "get_memory_stats" '{}')
check "get_memory_stats" "$R" "working_set"

echo ""
echo "=== VERIFICATION ==="

R=$(call_tool "verify_state" '{"frontend_expr":"JSON.stringify({title: document.title})","backend_state":{"title":"TestApp"}}')
check "verify_state (match)" "$R" "passed"

R=$(call_tool "assert_semantic" '{"expression":"document.title","label":"title","condition":"equals","expected":"TestApp"}')
check "assert_semantic (equals)" "$R" "passed"

R=$(call_tool "assert_semantic" '{"expression":"1+1","label":"math","condition":"equals","expected":2}')
check "assert_semantic (math)" "$R" "passed"

echo ""
echo "=== STREAMING ==="

R=$(call_tool "get_event_stream" '{}')
check "get_event_stream" "$R" "content"

echo ""
echo "=== INTENT ==="

R=$(call_tool "resolve_command" '{"query":"show settings"}')
check "resolve_command" "$R" "content"

echo ""
echo "=== WAIT_FOR ==="

R=$(call_tool "wait_for" '{"condition":"selector","value":"body","timeout_ms":3000}')
check "wait_for (selector body)" "$R" "ok"

R=$(call_tool "wait_for" '{"condition":"ipc_idle","timeout_ms":3000}')
check "wait_for (ipc_idle)" "$R" "ok"

R=$(call_tool "wait_for" '{"condition":"network_idle","timeout_ms":3000}')
check "wait_for (network_idle)" "$R" "ok"

R=$(call_tool "wait_for" '{"condition":"selector_gone","value":"#nonexistent-xyz-999","timeout_ms":2000}')
check "wait_for (selector_gone)" "$R" "ok"

echo ""
echo "=== TIME-TRAVEL ==="

R=$(call_tool "start_recording" '{}')
check "start_recording" "$R" "started"

R=$(call_tool "checkpoint" '{"id":"cp1","label":"before","state":{"step":"pre"}}')
check "checkpoint cp1" "$R" "created"

R=$(call_tool "list_checkpoints" '{}')
check "list_checkpoints" "$R" "cp1"

R=$(call_tool "get_recorded_events" '{}')
check "get_recorded_events" "$R" "content"

R=$(call_tool "get_replay_sequence" '{}')
check "get_replay_sequence" "$R" "content"

R=$(call_tool "checkpoint" '{"id":"cp2","label":"after","state":{"step":"post"}}')
check "checkpoint cp2" "$R" "created"

R=$(call_tool "events_between_checkpoints" '{"from_checkpoint":"cp1","to_checkpoint":"cp2"}')
check "events_between_checkpoints" "$R" "content"

R=$(call_tool "stop_recording" '{}')
check "stop_recording" "$R" "events"

echo ""
echo "=== CSS / STYLE INTROSPECTION ==="

R=$(call_tool "get_styles" '{"ref_id":"e3"}')
check "get_styles (default props)" "$R" "styles"

R=$(call_tool "get_styles" '{"ref_id":"e3","properties":["display","color","font-size"]}')
check "get_styles (specific props)" "$R" "display"

R=$(call_tool "get_bounding_boxes" '{"ref_ids":["e3","e4"]}')
check "get_bounding_boxes" "$R" "width"

echo ""
echo "=== VISUAL DEBUG OVERLAYS ==="

R=$(call_tool "highlight_element" '{"ref_id":"e3","color":"rgba(0,120,255,0.3)","label":"test-highlight"}')
check "highlight_element" "$R" "ok"

R=$(call_tool "clear_highlights" '{}')
check "clear_highlights" "$R" "ok"

echo ""
echo "=== CSS INJECTION ==="

R=$(call_tool "inject_css" '{"css":".__victauri_test_marker__ { display: block; }"}')
check "inject_css" "$R" "ok"

R=$(call_tool "remove_injected_css" '{}')
check "remove_injected_css" "$R" "ok"

echo ""
echo "=== ACCESSIBILITY AUDIT ==="

R=$(call_tool "audit_accessibility" '{}')
check "audit_accessibility" "$R" "summary"

echo ""
echo "=== PERFORMANCE METRICS ==="

R=$(call_tool "get_performance_metrics" '{}')
check "get_performance_metrics (navigation)" "$R" "navigation"

R=$(call_tool "get_performance_metrics" '{}')
check "get_performance_metrics (resources)" "$R" "resources"

R=$(call_tool "get_performance_metrics" '{}')
check "get_performance_metrics (dom)" "$R" "dom"

echo ""
echo "=== RESOURCES ==="

read_resource() {
  local uri="$1"
  local id=$((RANDOM % 9000 + 1000))
  curl -s -X POST "$BASE/mcp" \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    -H "mcp-session-id: $MCP_SESSION" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"resources/read\",\"params\":{\"uri\":\"$uri\"}}"
}

R=$(read_resource "victauri://state")
check "resource: state" "$R" "commands_registered"

R=$(read_resource "victauri://windows")
check "resource: windows" "$R" "main"

R=$(read_resource "victauri://ipc-log")
check "resource: ipc-log" "$R" "content"

echo ""
echo "=========================================="
echo "  RESULTS: $PASS passed, $FAIL failed"
echo "=========================================="
if [ $FAIL -gt 0 ]; then
  echo ""
  echo "Failures:"
  echo -e "$ERRORS"
fi
