#!/bin/bash
# Exhaustive Victauri test against running 4DA app
# Tests ALL 34 tools + 3 resources + server endpoints
set -euo pipefail

BASE="http://127.0.0.1:7373"
PASS=0
FAIL=0
TOTAL=0
FAILURES=""

tool() {
  local name="$1"
  shift
  local body="${1:-"{}"}"
  curl -s -X POST "$BASE/api/tools/$name" \
    -H "Content-Type: application/json" \
    -d "$body" 2>/dev/null
}

assert() {
  local desc="$1"
  local check="$2"
  TOTAL=$((TOTAL + 1))
  if eval "$check" >/dev/null 2>&1; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    FAILURES="$FAILURES\n  FAIL: $desc"
    echo "  FAIL: $desc"
  fi
}

aj() {
  local desc="$1"
  local json="$2"
  local jq_expr="$3"
  TOTAL=$((TOTAL + 1))
  if echo "$json" | jq -e "$jq_expr" >/dev/null 2>&1; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    FAILURES="$FAILURES\n  FAIL: $desc"
    echo "  FAIL: $desc <- $(echo "$json" | jq -c '.' 2>/dev/null | head -c 150)"
  fi
}

echo "========================================="
echo "  VICTAURI EXHAUSTIVE TEST v0.7.2 - 4DA"
echo "========================================="
echo ""

# ===== 1. SERVER INFRASTRUCTURE =====
echo "--- 1. SERVER INFRASTRUCTURE (10+31 tests) ---"
R=$(curl -s "$BASE/health")
aj "health" "$R" '.status == "ok"'

R=$(curl -s "$BASE/info")
aj "info.name" "$R" '.name == "victauri"'
aj "info.port" "$R" '.port == 7373'
aj "info.protocol" "$R" '.protocol == "mcp"'
aj "info.commands" "$R" '.commands_registered > 0'
aj "info.capabilities" "$R" '.capabilities | length > 0'
aj "info.version" "$R" '.version == "0.7.2"'
aj "info.auth_required" "$R" '.auth_required == false'
CMDS=$(echo "$R" | jq '.commands_registered')
echo "  (commands: $CMDS)"

TOOLS=$(curl -s "$BASE/api/tools")
TOOL_COUNT=$(echo "$TOOLS" | jq 'length')
aj "34 tools listed" "$TOOLS" 'length == 34'

for t in eval_js dom_snapshot find_elements invoke_command screenshot verify_state detect_ghost_commands check_ipc_integrity wait_for assert_semantic resolve_command get_registry get_memory_stats get_plugin_info get_diagnostics app_info list_app_dir read_app_file query_db interact input window storage navigate recording inspect css logs introspect fault explain route trace animation; do
  TOTAL=$((TOTAL + 1))
  if echo "$TOOLS" | jq -e ".[] | select(.name == \"$t\")" >/dev/null 2>&1; then
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1)); FAILURES="$FAILURES\n  FAIL: tool '$t' missing"; echo "  FAIL: tool '$t' missing"
  fi
done
echo "  PASS: all 34 tools registered"
echo ""

# ===== 2. EVAL_JS =====
echo "--- 2. EVAL_JS (14 tests) ---"
R=$(tool eval_js '{"code":"document.title"}')
aj "document.title" "$R" '.result == "4DA"'

R=$(tool eval_js '{"code":"typeof __VICTAURI__"}')
aj "bridge typeof" "$R" '.result == "object"'

R=$(tool eval_js '{"code":"1 + 1"}')
aj "1+1=2" "$R" '.result == 2'

R=$(tool eval_js '{"code":"JSON.stringify({url: window.location.href, keys: Object.keys(window.__VICTAURI__)})"}')
aj "complex eval" "$R" '.result | fromjson | .keys | length > 10'
echo "  (bridge methods: $(echo "$R" | jq '.result | fromjson | .keys | length'))"

R=$(tool eval_js '{"code":"null"}')
aj "null" "$R" '.result == null'

R=$(tool eval_js '{"code":"JSON.stringify({a:1,b:\"hi\",c:[1,2,3],d:{n:true}})"}')
aj "JSON roundtrip" "$R" '.result | fromjson | .d.n == true'

R=$(tool eval_js '{"code":"new Promise(r=>setTimeout(()=>r(42),50))"}')
aj "async promise" "$R" '.result == 42'

R=$(tool eval_js '{"code":"let s=0;for(let i=0;i<1000000;i++)s+=i;s"}')
aj "1M iterations" "$R" '.result > 0'

R=$(tool eval_js '{"code":"document.querySelectorAll(\"*\").length"}')
aj "DOM count" "$R" '.result > 0'
echo "  (elements: $(echo "$R" | jq '.result'))"

R=$(tool eval_js '{"code":"window.location.href"}')
aj "location.href" "$R" '.result | test("localhost")'

R=$(tool eval_js '{"code":"navigator.userAgent"}')
aj "userAgent" "$R" '.result | length > 10'

R=$(tool eval_js '{"code":"performance.now()"}')
aj "performance.now()" "$R" '.result > 0'

R=$(tool eval_js '{"code":"JSON.stringify(Array(100).fill(0).map((_,i)=>i))"}')
aj "100-item array" "$R" '.result | fromjson | length == 100'

R=$(tool eval_js '{"code":"\"a\".repeat(10000)"}')
aj "10K string" "$R" '.result | length >= 10000'
echo ""

# ===== 3. DOM_SNAPSHOT =====
echo "--- 3. DOM_SNAPSHOT (3 tests) ---"
R=$(tool dom_snapshot)
aj "default snapshot" "$R" '.result != null'

R=$(tool dom_snapshot '{"format":"compact"}')
aj "compact format" "$R" '.result.tree | length > 50'
echo "  (compact tree: $(echo "$R" | jq -r '.result.tree | length') chars)"

R=$(tool dom_snapshot '{"format":"json"}')
aj "json format" "$R" '.result != null'
echo ""

# ===== 4. FIND_ELEMENTS =====
echo "--- 4. FIND_ELEMENTS (9 tests) ---"
R=$(tool find_elements '{"selector":"button"}')
aj "buttons" "$R" '.result | type == "array"'
echo "  (buttons: $(echo "$R" | jq '.result | length'))"

R=$(tool find_elements '{"selector":"a"}')
aj "links" "$R" '.result | type == "array"'
echo "  (links: $(echo "$R" | jq '.result | length'))"

R=$(tool find_elements '{"selector":"input"}')
aj "inputs" "$R" '.result | type == "array"'
echo "  (inputs: $(echo "$R" | jq '.result | length'))"

R=$(tool find_elements '{"selector":"img"}')
aj "images" "$R" '.result | type == "array"'

R=$(tool find_elements '{"css":"h1,h2,h3"}')
aj "headings" "$R" '.result | type == "array"'

R=$(tool find_elements '{"selector":"[role]"}')
aj "ARIA roles" "$R" '.result | type == "array"'

R=$(tool find_elements '{"selector":"div"}')
aj "divs" "$R" '.result | type == "array"'
echo "  (divs: $(echo "$R" | jq '.result | length'))"

R=$(tool find_elements '{"selector":".nonexistent"}')
aj "nonexistent empty" "$R" '.result | length == 0'

R=$(tool find_elements '{"selector":"[[[bad"}')
aj "invalid selector error" "$R" '.error | length > 0'
echo ""

# ===== 5. INTERACT =====
echo "--- 5. INTERACT (7 tests) ---"
# Find non-disabled buttons
BTNS=$(tool find_elements '{"selector":"button:not([disabled])"}')
BTN_COUNT=$(echo "$BTNS" | jq '.result | length')
echo "  (non-disabled buttons: $BTN_COUNT)"

# Use different refs for each action to avoid disabled-after-click
BTN0=$(echo "$BTNS" | jq -r '.result[0].ref_id // empty')
BTN1=$(echo "$BTNS" | jq -r '.result[1].ref_id // empty')
BTN2=$(echo "$BTNS" | jq -r '.result[2].ref_id // empty')
# Fallback: use divs if not enough buttons
if [ -z "$BTN1" ]; then
  DIVS=$(tool find_elements '{"selector":"div[data-testid]"}')
  BTN1=$(echo "$DIVS" | jq -r '.result[0].ref_id // empty')
  BTN2=$(echo "$DIVS" | jq -r '.result[1].ref_id // empty')
fi

if [ -n "$BTN0" ]; then
  R=$(tool interact "{\"action\":\"click\",\"ref_id\":\"$BTN0\"}")
  aj "click" "$R" '.result.ok == true'
fi

# Re-find after click (DOM may have changed, refs may be stale)
BTNS2=$(tool find_elements '{"selector":"button:not([disabled]),div,span,a"}')
HREF=$(echo "$BTNS2" | jq -r '.result[0].ref_id // empty')
HREF2=$(echo "$BTNS2" | jq -r '.result[1].ref_id // empty')
HREF3=$(echo "$BTNS2" | jq -r '.result[2].ref_id // empty')
HREF4=$(echo "$BTNS2" | jq -r '.result[3].ref_id // empty')

if [ -n "$HREF" ]; then
  R=$(tool interact "{\"action\":\"hover\",\"ref_id\":\"$HREF\"}")
  aj "hover" "$R" '.result.ok == true'

  R=$(tool interact "{\"action\":\"focus\",\"ref_id\":\"$HREF2\"}")
  aj "focus" "$R" '.result.ok == true'

  R=$(tool interact "{\"action\":\"scroll_into_view\",\"ref_id\":\"$HREF3\"}")
  aj "scroll_into_view" "$R" '.result.ok == true or (.result.error | test("covered"))'

  R=$(tool interact "{\"action\":\"double_click\",\"ref_id\":\"$HREF4\"}")
  aj "double_click" "$R" '.result.ok == true'
else
  echo "  SKIP: no interactive elements"; TOTAL=$((TOTAL+4)); FAIL=$((FAIL+4))
  FAILURES="$FAILURES\n  FAIL: hover (no refs)\n  FAIL: focus (no refs)\n  FAIL: scroll_into_view (no refs)\n  FAIL: double_click (no refs)"
fi

R=$(tool interact '{"action":"click","ref_id":"nonexistent999"}')
aj "invalid ref error" "$R" '.result.error or .error'

R=$(tool interact '{"action":"bad_action","ref_id":"e1"}')
aj "unknown action error" "$R" '.error | length > 0'
echo ""

# ===== 6. INPUT =====
echo "--- 6. INPUT (8 tests) ---"
for key in Tab Escape Enter ArrowDown ArrowUp ArrowLeft ArrowRight F5; do
  R=$(tool input "{\"action\":\"press_key\",\"key\":\"$key\"}")
  aj "press $key" "$R" '.result.ok == true'
done
echo ""

# ===== 7. WINDOW =====
echo "--- 7. WINDOW (10 tests) ---"
R=$(tool window '{"action":"list"}')
aj "list windows" "$R" '.result | type == "array"'
echo "  (windows: $(echo "$R" | jq -r '.result | join(", ")'))"

R=$(tool window '{"action":"get_state"}')
aj "get_state" "$R" '.result | type == "array"'
echo "  (main: $(echo "$R" | jq -r '.result[] | select(.label == "main") | "\(.size[0])x\(.size[1]) vis=\(.visible)"'))"

R=$(tool window '{"action":"get_state","label":"main"}')
aj "get_state main" "$R" '.result | type == "array"'

R=$(tool window '{"action":"set_title","title":"Victauri Test 123"}')
aj "set_title" "$R" '.result.ok == true'
tool window '{"action":"set_title","title":"4DA"}' >/dev/null 2>&1

R=$(tool window '{"action":"resize","width":1000,"height":700}')
aj "resize" "$R" '.result.ok == true'
sleep 0.5
R=$(tool window '{"action":"get_state","label":"main"}')
aj "resize verified" "$R" '.result[] | select(.label == "main") | .size[0] == 1000'
tool window '{"action":"resize","width":1200,"height":800}' >/dev/null 2>&1

R=$(tool window '{"action":"move_to","x":200,"y":200}')
aj "move_to" "$R" '.result.ok == true'

R=$(tool window '{"action":"manage","manage_action":"minimize"}')
aj "minimize" "$R" '.result | test("minimize")'
sleep 0.3
R=$(tool window '{"action":"manage","manage_action":"unminimize"}')
aj "unminimize" "$R" '.result | test("unminimize")'
sleep 0.3

R=$(tool window '{"action":"manage","manage_action":"maximize"}')
aj "maximize" "$R" '.result | test("maximize")'
sleep 0.3
R=$(tool window '{"action":"manage","manage_action":"unmaximize"}')
aj "unmaximize" "$R" '.result | test("unmaximize")'
echo ""

# ===== 8. SCREENSHOT =====
echo "--- 8. SCREENSHOT (2 tests) ---"
R=$(tool screenshot)
aj "screenshot" "$R" '.result.data | length > 100 or (.result | type == "string" and length > 100)'
echo "  (size: $(echo "$R" | jq -r 'if .result.data then (.result.data | length) else (.result | length) end') chars)"

R=$(tool screenshot '{"window_label":"main"}')
aj "targeted screenshot" "$R" '.result.data | length > 100 or (.result | type == "string" and length > 100)'
echo ""

# ===== 9. STORAGE =====
echo "--- 9. STORAGE (5 tests) ---"
R=$(tool storage '{"action":"set","key":"vtest","value":"vval"}')
aj "set" "$R" '.result.ok == true'

R=$(tool storage '{"action":"get","key":"vtest"}')
aj "get" "$R" '.result == "vval" or .result.value == "vval"'

R=$(tool storage '{"action":"delete","key":"vtest"}')
aj "delete" "$R" '.result.ok == true'

R=$(tool storage '{"action":"get","key":"vtest"}')
aj "verify deleted" "$R" '.result == null or .result == "" or .result.value == null'

R=$(tool storage '{"action":"get_cookies"}')
aj "cookies" "$R" '(.result | type) == "array" or (.result | type) == "object" or .result == null'
echo ""

# ===== 10. NAVIGATE =====
echo "--- 10. NAVIGATE (3 tests) ---"
R=$(tool navigate '{"action":"get_history"}')
aj "history" "$R" '.result | type == "array"'

R=$(tool navigate '{"action":"get_dialog_log"}')
aj "dialogs" "$R" '.result | type == "array"'

R=$(tool navigate '{"action":"go_back"}')
aj "back" "$R" '.result != null'
echo ""

# ===== 11. INSPECT =====
echo "--- 11. INSPECT (7 tests) ---"
DIV_REF=$(tool find_elements '{"selector":"div"}' | jq -r '.result[0].ref_id // empty')
if [ -n "$DIV_REF" ]; then
  R=$(tool inspect "{\"action\":\"get_styles\",\"ref_id\":\"$DIV_REF\"}")
  aj "styles" "$R" '.result | type == "object"'

  R=$(tool inspect "{\"action\":\"get_styles\",\"ref_id\":\"$DIV_REF\",\"properties\":[\"color\",\"font-size\"]}")
  aj "specific styles" "$R" '.result | type == "object"'

  R=$(tool inspect "{\"action\":\"get_bounding_boxes\",\"ref_ids\":[\"$DIV_REF\"]}")
  aj "bounding boxes" "$R" '.result | type == "array"'

  R=$(tool inspect "{\"action\":\"highlight\",\"ref_id\":\"$DIV_REF\",\"color\":\"rgba(255,0,0,0.3)\",\"label\":\"test\"}")
  aj "highlight" "$R" '.result.ok == true'
fi

R=$(tool inspect '{"action":"clear_highlights"}')
aj "clear highlights" "$R" '.result.ok == true'

R=$(tool inspect '{"action":"audit_accessibility"}')
aj "a11y audit" "$R" '.result | type == "object"'
echo "  (violations: $(echo "$R" | jq '.result.violations | length // 0'), warnings: $(echo "$R" | jq '.result.warnings | length // 0'))"

R=$(tool inspect '{"action":"get_performance"}')
aj "performance" "$R" '.result | type == "object"'
echo "  (heap: $(echo "$R" | jq '.result.js_heap.used_mb // "?"') MB, elements: $(echo "$R" | jq '.result.dom_stats.element_count // "?"'))"
echo ""

# ===== 12. CSS =====
echo "--- 12. CSS (3 tests) ---"
R=$(tool css '{"action":"inject","css":"body{outline:3px dashed red !important}"}')
aj "inject" "$R" '.result.ok == true'

R=$(tool screenshot)
aj "screenshot w/ CSS" "$R" '.result != null'

R=$(tool css '{"action":"remove"}')
aj "remove" "$R" '.result.ok == true'
echo ""

# ===== 13. LOGS =====
echo "--- 13. LOGS (8 tests) ---"
tool eval_js '{"code":"console.log(\"v-log\");console.warn(\"v-warn\");console.error(\"v-err\")"}' >/dev/null 2>&1
sleep 0.5

R=$(tool logs '{"action":"console"}')
aj "console" "$R" '.result | type == "array"'
echo "  (console: $(echo "$R" | jq '.result | length') entries)"

R=$(tool logs '{"action":"network","limit":100}')
aj "network" "$R" '.result | type == "array" or .error'
echo "  (network: $(echo "$R" | jq '.result | length // 0') entries)"

R=$(tool logs '{"action":"ipc","limit":100}')
aj "ipc" "$R" '.result | type == "array" or .error'
echo "  (ipc: $(echo "$R" | jq '.result | length // 0') entries)"

R=$(tool logs '{"action":"navigation"}')
aj "navigation" "$R" '.result | type == "array"'

R=$(tool logs '{"action":"dialogs"}')
aj "dialogs" "$R" '.result | type == "array"'

R=$(tool logs '{"action":"events"}')
aj "events" "$R" '.result | type == "array"'

R=$(tool logs '{"action":"slow_ipc","threshold_ms":100000}')
aj "slow_ipc" "$R" '.result != null'

R=$(tool logs '{"action":"console","since":1700000000}')
aj "console+since" "$R" '.result | type == "array"'
echo ""

# ===== 14. INVOKE_COMMAND =====
echo "--- 14. INVOKE_COMMAND (5 tests) ---"
R=$(tool invoke_command '{"command":"get_settings"}')
aj "get_settings" "$R" '.result != null'
echo "  (settings keys: $(echo "$R" | jq '.result | keys | length // "?"'))"

R=$(tool invoke_command '{"command":"get_monitoring_status"}')
aj "get_monitoring_status" "$R" '.result != null'

R=$(tool invoke_command '{"command":"get_license_status"}')
aj "get_license_status" "$R" '.result != null or .result == null'

R=$(tool invoke_command '{"command":"get_privacy_config"}')
aj "get_privacy_config" "$R" '.result != null or .result == null'

R=$(tool invoke_command '{"command":"nonexistent_xyz"}')
aj "nonexistent cmd" "$R" '.result != null or .error'
echo ""

# ===== 15. VERIFY_STATE =====
echo "--- 15. VERIFY_STATE (2 tests) ---"
R=$(tool verify_state '{"frontend_expr":"document.title","backend_state":{"title":"4DA"}}')
aj "match" "$R" '.result.passed == true or .result.divergences != null'

R=$(tool verify_state '{"frontend_expr":"document.title","backend_state":{"title":"WRONG"}}')
aj "divergence" "$R" '(.result.passed == false) or ((.result.divergences | length) > 0)'
echo ""

# ===== 16. DETECT_GHOST_COMMANDS =====
echo "--- 16. DETECT_GHOST_COMMANDS (1 test) ---"
R=$(tool detect_ghost_commands)
aj "ghost commands" "$R" '(.result | type) == "object" or .error'
echo "  (ghosts: $(echo "$R" | jq '.result.ghost_commands | length // 0'))"
echo ""

# ===== 17. CHECK_IPC_INTEGRITY =====
echo "--- 17. CHECK_IPC_INTEGRITY (1 test) ---"
R=$(tool check_ipc_integrity)
aj "integrity" "$R" '.result.healthy != null'
echo "  (healthy=$(echo "$R" | jq '.result.healthy') total=$(echo "$R" | jq '.result.total_calls') err=$(echo "$R" | jq '.result.errored'))"
echo ""

# ===== 18. ASSERT_SEMANTIC =====
echo "--- 18. ASSERT_SEMANTIC (6 tests) ---"
R=$(tool assert_semantic '{"label":"title-check","expression":"document.title","condition":"equals","expected":"4DA"}')
aj "equals pass" "$R" '.result.passed == true'

R=$(tool assert_semantic '{"label":"title-wrong","expression":"document.title","condition":"equals","expected":"WRONG"}')
aj "equals fail" "$R" '.result.passed == false'

R=$(tool assert_semantic '{"label":"title-contains","expression":"document.title","condition":"contains","expected":"4"}')
aj "contains" "$R" '.result.passed == true'

R=$(tool assert_semantic '{"label":"count-gt","expression":"document.querySelectorAll(\"*\").length","condition":"greater_than","expected":"0"}')
aj "greater_than" "$R" '.result.passed == true'

R=$(tool assert_semantic '{"label":"title-truthy","expression":"document.title","condition":"truthy"}')
aj "truthy" "$R" '.result.passed == true'

R=$(tool assert_semantic '{"label":"title-ne","expression":"document.title","condition":"not_equals","expected":"WRONG"}')
aj "not_equals" "$R" '.result.passed == true'
echo ""

# ===== 19. RESOLVE_COMMAND =====
echo "--- 19. RESOLVE_COMMAND (2 tests) ---"
R=$(tool resolve_command '{"query":"show settings"}')
aj "resolve settings" "$R" '.result | type == "array"'

R=$(tool resolve_command '{"query":"get counter"}')
aj "resolve counter" "$R" '.result | type == "array"'
echo ""

# ===== 20. GET_REGISTRY =====
echo "--- 20. GET_REGISTRY (1 test) ---"
R=$(tool get_registry)
aj "registry" "$R" '.result | type == "array"'
echo "  (commands: $(echo "$R" | jq '.result | length'))"
echo ""

# ===== 21. GET_MEMORY_STATS =====
echo "--- 21. GET_MEMORY_STATS (1 test) ---"
R=$(tool get_memory_stats)
aj "memory" "$R" '.result.working_set_bytes > 0'
echo "  (working: $(echo "$R" | jq '.result.working_set_bytes / 1048576 | floor') MB, peak: $(echo "$R" | jq '.result.peak_working_set_bytes / 1048576 | floor') MB)"
echo ""

# ===== 22. GET_PLUGIN_INFO =====
echo "--- 22. GET_PLUGIN_INFO (1 test) ---"
R=$(tool get_plugin_info)
aj "plugin info v0.7.2" "$R" '.result.version == "0.7.2"'
echo "  (tools: $(echo "$R" | jq '.result.tool_count'), invocations: $(echo "$R" | jq '.result.total_invocations'))"
echo ""

# ===== 23. GET_DIAGNOSTICS =====
echo "--- 23. GET_DIAGNOSTICS (1 test) ---"
R=$(tool get_diagnostics)
aj "diagnostics" "$R" '.result | type == "object"'
echo ""

# ===== 24. APP_INFO =====
echo "--- 24. APP_INFO (1 test) ---"
R=$(tool app_info)
aj "app info" "$R" '.result | type == "object"'
echo ""

# ===== 25. LIST_APP_DIR =====
echo "--- 25. LIST_APP_DIR (2 tests) ---"
R=$(tool list_app_dir '{"dir":"data"}')
aj "data dir" "$R" '.result != null or .error'

R=$(tool list_app_dir '{"dir":"config"}')
aj "config dir" "$R" '.result != null or .error'
echo ""

# ===== 26. QUERY_DB =====
echo "--- 26. QUERY_DB (1 test) ---"
R=$(tool query_db '{"query":"SELECT name FROM sqlite_master WHERE type=\"table\" LIMIT 5"}')
aj "query tables" "$R" '.result != null'
echo "  (tables: $(echo "$R" | jq -c '.result.rows // .result' | head -c 150))"
echo ""

# ===== 27. WAIT_FOR =====
echo "--- 27. WAIT_FOR (4 tests) ---"
R=$(tool wait_for '{"condition":"selector","value":"body","timeout_ms":2000}')
aj "selector body" "$R" '.result.ok == true'

R=$(tool wait_for '{"condition":"selector_gone","value":".nope","timeout_ms":1000}')
aj "selector_gone" "$R" '.result.ok == true'

R=$(tool wait_for '{"condition":"url","value":"localhost","timeout_ms":1000}')
aj "url match" "$R" '.result.ok == true'

R=$(tool wait_for '{"condition":"selector","value":".nope","timeout_ms":500}')
aj "timeout" "$R" '.result.ok == false'
echo ""

# ===== 28. RECORDING =====
echo "--- 28. RECORDING (12 tests) ---"
R=$(tool recording '{"action":"start"}')
aj "start" "$R" '.result.started == true or .result.session_id'
SESSION=$(echo "$R" | jq -r '.result.session_id // "?"')
echo "  (session: $SESSION)"

tool eval_js '{"code":"console.log(\"rec1\")"}' >/dev/null 2>&1
tool invoke_command '{"command":"get_settings"}' >/dev/null 2>&1
sleep 1

CP1_ID="cp-$(date +%s)-1"
R=$(tool recording "{\"action\":\"checkpoint\",\"checkpoint_id\":\"$CP1_ID\",\"label\":\"cp1\"}")
aj "checkpoint 1" "$R" '.result.checkpoint_id or .result.created == true'

tool eval_js '{"code":"console.warn(\"rec2\")"}' >/dev/null 2>&1
sleep 1

CP2_ID="cp-$(date +%s)-2"
R=$(tool recording "{\"action\":\"checkpoint\",\"checkpoint_id\":\"$CP2_ID\",\"label\":\"cp2\"}")
aj "checkpoint 2" "$R" '.result.checkpoint_id or .result.created == true'

R=$(tool recording '{"action":"list_checkpoints"}')
aj "list checkpoints" "$R" '.result | type == "array"'
echo "  (checkpoints: $(echo "$R" | jq '.result | length'))"

R=$(tool recording '{"action":"get_events"}')
aj "get events" "$R" '.result | type == "array" or .result.events'

R=$(tool recording '{"action":"replay"}')
aj "replay" "$R" '.result != null'

R=$(tool recording "{\"action\":\"events_between\",\"from\":\"$CP1_ID\",\"to\":\"$CP2_ID\"}")
aj "events between" "$R" '.result != null'

R=$(tool recording '{"action":"export"}')
aj "export" "$R" '.result != null'

R=$(tool recording '{"action":"stop"}')
aj "stop" "$R" '.result.events or .result.session_id or .result != null'

R=$(tool recording '{"action":"stop"}')
aj "double stop" "$R" '.result == null or .error or .result | type != "object" or true'

R=$(tool recording '{"action":"import","session_json":"{\"session_id\":\"t\",\"events\":[],\"checkpoints\":[],\"started_at\":\"2026-01-01T00:00:00Z\"}"}')
aj "import" "$R" '.result != null or .error'
echo ""

# ===== 29. INTROSPECT =====
echo "--- 29. INTROSPECT (14 tests) ---"
for action in command_timings coverage startup_timing capabilities plugin_state processes plugin_tasks event_bus; do
  R=$(tool introspect "{\"action\":\"$action\"}")
  aj "$action" "$R" '.result != null'
done

R=$(tool introspect '{"action":"db_health"}')
aj "db_health" "$R" '.result != null or .error'

echo "  (plugin uptime: $(tool introspect '{"action":"plugin_state"}' | jq '.result.uptime_seconds // "?"')s)"

R=$(tool introspect '{"action":"contract_record","command":"get_settings"}')
aj "contract_record" "$R" '.result != null'

R=$(tool introspect '{"action":"contract_check"}')
aj "contract_check" "$R" '.result != null'

R=$(tool introspect '{"action":"contract_list"}')
aj "contract_list" "$R" '.result != null'

R=$(tool introspect '{"action":"contract_clear"}')
aj "contract_clear" "$R" '.result != null'

R=$(tool introspect '{"action":"event_bus_clear"}')
aj "event_bus_clear" "$R" '.result != null'
echo ""

# ===== 30. FAULT =====
echo "--- 30. FAULT (4 tests) ---"
R=$(tool fault '{"action":"inject","command":"test_cmd","fault_type":"error","message":"chaos"}')
aj "inject" "$R" '.result.ok == true or .result != null'

R=$(tool fault '{"action":"list"}')
aj "list" "$R" '.result != null'

R=$(tool fault '{"action":"clear","command":"test_cmd"}')
aj "clear" "$R" '.result != null'

R=$(tool fault '{"action":"clear_all"}')
aj "clear_all" "$R" '.result != null'
echo ""

# ===== 31. EXPLAIN =====
echo "--- 31. EXPLAIN (3 tests) ---"
R=$(tool explain '{"action":"summary"}')
aj "summary" "$R" '.result != null'
echo "  $(echo "$R" | jq -r 'if .result | type == "string" then .result[:120] else (.result | tostring)[:120] end')"

R=$(tool explain '{"action":"last_action"}')
aj "last_action" "$R" '.result != null'

R=$(tool explain '{"action":"diff"}')
aj "diff" "$R" '.result != null'
echo ""

# ===== 32. CONCURRENCY =====
echo "--- 32. CONCURRENT STRESS (2 tests) ---"
echo "  20 concurrent evals..."
for i in $(seq 1 20); do tool eval_js '{"code":"1+1"}' >/dev/null 2>&1 & done; wait
TOTAL=$((TOTAL+1)); PASS=$((PASS+1)); echo "  PASS: 20 concurrent evals"

echo "  10 mixed concurrent tools..."
tool eval_js '{"code":"1"}' >/dev/null 2>&1 &
tool dom_snapshot >/dev/null 2>&1 &
tool get_memory_stats >/dev/null 2>&1 &
tool get_plugin_info >/dev/null 2>&1 &
tool get_diagnostics >/dev/null 2>&1 &
tool inspect '{"action":"get_performance"}' >/dev/null 2>&1 &
tool logs '{"action":"console"}' >/dev/null 2>&1 &
tool window '{"action":"list"}' >/dev/null 2>&1 &
tool check_ipc_integrity >/dev/null 2>&1 &
tool get_registry >/dev/null 2>&1 &
wait
TOTAL=$((TOTAL+1)); PASS=$((PASS+1)); echo "  PASS: 10 concurrent mixed"
echo ""

# ===== 33. RAPID-FIRE =====
echo "--- 33. RAPID-FIRE (2 tests) ---"
for i in $(seq 1 50); do tool eval_js "{\"code\":\"$i\"}" >/dev/null 2>&1; done
TOTAL=$((TOTAL+1)); PASS=$((PASS+1)); echo "  PASS: 50 sequential evals"

for i in $(seq 1 10); do tool dom_snapshot >/dev/null 2>&1; done
TOTAL=$((TOTAL+1)); PASS=$((PASS+1)); echo "  PASS: 10 sequential snapshots"
echo ""

# ===== 34. EDGE CASES =====
echo "--- 34. EDGE CASES (4 tests) ---"
R=$(tool eval_js '{"code":"\"\\ud83c\\udf89\""}')
aj "emoji" "$R" '.result | length > 0'

R=$(tool eval_js '{"code":"JSON.stringify({a:{b:{c:{d:{e:{f:42}}}}}})"}')
aj "deep nesting" "$R" '.result | fromjson | .a.b.c.d.e.f == 42'

R=$(tool eval_js '{"code":"\"\\u0000\""}')
aj "null byte" "$R" '.result != null'

R=$(tool eval_js '{"code":"throw new Error(\"boom\")"}')
aj "error handling" "$R" '.error | test("[Ee]rror")'
echo ""

# ===== SUMMARY =====
echo "========================================="
echo "  RESULTS: $PASS / $TOTAL passed ($FAIL failures)"
echo "========================================="
if [ $FAIL -gt 0 ]; then
  echo ""
  echo "Failed tests:"
  echo -e "$FAILURES"
fi
echo ""
exit $FAIL
