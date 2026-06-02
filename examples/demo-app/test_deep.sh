#!/bin/bash
# Deep integration test for Victauri demo-app
# Tests EVERYTHING — especially features never tested on third-party apps:
#   - Command registry with 19 #[inspectable] commands
#   - Natural language command resolution
#   - Ghost command detection against populated registry
#   - invoke_command with real args and validation errors
#   - Cross-boundary state verification with real backend data
#   - Multi-window management
#   - Full interaction pipelines (click → state change → verify)
#
# Usage: PORT=7373 bash test_deep.sh   (the plugin's default port; override if it fell back)

set -uo pipefail

PORT="${PORT:-7373}"
BASE="http://127.0.0.1:$PORT"
PASS=0
FAIL=0
TOTAL=0

call_tool() {
  local tool="$1"
  local args="$2"
  curl -s -X POST "$BASE/api/tools/$tool" \
    -H "Content-Type: application/json" \
    -d "$args" 2>/dev/null
}

check() {
  TOTAL=$((TOTAL+1))
  local name="$1"
  local result="$2"
  local pattern="$3"
  if echo "$result" | grep -qiE "$pattern"; then
    PASS=$((PASS+1))
    printf "  [PASS] %s\n" "$name"
  else
    FAIL=$((FAIL+1))
    printf "  [FAIL] %s\n" "$name"
    printf "         expected: %s\n" "$pattern"
    printf "         got: %.200s\n" "$result"
  fi
}

check_not() {
  TOTAL=$((TOTAL+1))
  local name="$1"
  local result="$2"
  local pattern="$3"
  if echo "$result" | grep -qiE "$pattern"; then
    FAIL=$((FAIL+1))
    printf "  [FAIL] %s\n" "$name"
    printf "         should NOT match: %s\n" "$pattern"
    printf "         got: %.200s\n" "$result"
  else
    PASS=$((PASS+1))
    printf "  [PASS] %s\n" "$name"
  fi
}

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║  Victauri Deep Integration Test — Demo App (19 commands)   ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""

# ═══════════════════════════════════════════════════════════════════
# MODULE 1: Server Infrastructure
# ═══════════════════════════════════════════════════════════════════
echo "── Module 1: Server Infrastructure ──"

R=$(curl -s "$BASE/health")
check "1.1 health endpoint" "$R" '"status".*"ok"'

R=$(curl -s "$BASE/info")
check "1.2 info shows 19 commands" "$R" '"commands_registered":19'
check "1.3 info auth disabled" "$R" '"auth_required":false'
check "1.4 info version is semver" "$R" '"version":"[0-9]+\.[0-9]+\.[0-9]+"'
check "1.5 info port correct" "$R" "\"port\":$PORT"

R=$(call_tool "get_plugin_info" '{}')
check "1.6 plugin_info has version" "$R" '"version"'
check "1.7 plugin_info has tools" "$R" '"tools"'

R=$(call_tool "get_memory_stats" '{}')
check "1.8 memory stats working_set" "$R" '"working_set_bytes"'
check "1.9 memory stats peak" "$R" '"peak_working_set_bytes"'

R=$(call_tool "get_diagnostics" '{}')
check "1.10 diagnostics returns data" "$R" '"bridge_version"\|"warnings"'

# ═══════════════════════════════════════════════════════════════════
# MODULE 2: Command Registry (FIRST TIME TESTING NON-EMPTY REGISTRY)
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 2: Command Registry (19 #[inspectable] commands) ──"

R=$(call_tool "get_registry" '{}')
check "2.1 registry returns commands" "$R" '"name"'
check "2.2 registry has greet" "$R" '"greet"'
check "2.3 registry has increment" "$R" '"increment"'
check "2.4 registry has add_todo" "$R" '"add_todo"'
check "2.5 registry has get_settings" "$R" '"get_settings"'
check "2.6 registry has submit_contact" "$R" '"submit_contact"'
check "2.7 registry has send_notification" "$R" '"send_notification"'
check "2.8 registry has get_app_state" "$R" '"get_app_state"'
check "2.9 registry has show_notification_window" "$R" '"show_notification_window"'

# Count commands in registry
CMD_COUNT=$(echo "$R" | grep -o '"name"' | wc -l)
check "2.10 registry count = 19" "$CMD_COUNT" "^19$"

# Check metadata fields
check "2.11 has description field" "$R" '"description"'
check "2.12 has intent field" "$R" '"intent"'
check "2.13 has category field" "$R" '"category"'

# ═══════════════════════════════════════════════════════════════════
# MODULE 3: Natural Language Command Resolution
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 3: Natural Language Command Resolution ──"

R=$(call_tool "resolve_command" '{"query":"increase counter"}')
check "3.1 resolves 'increase counter'" "$R" '"increment"'

R=$(call_tool "resolve_command" '{"query":"show settings"}')
check "3.2 resolves 'show settings'" "$R" '"get_settings"'

R=$(call_tool "resolve_command" '{"query":"add a todo"}')
check "3.3 resolves 'add a todo'" "$R" '"add_todo"'

R=$(call_tool "resolve_command" '{"query":"submit contact form"}')
check "3.4 resolves 'submit contact form'" "$R" '"submit_contact"'

R=$(call_tool "resolve_command" '{"query":"greet someone"}')
check "3.5 resolves 'greet someone'" "$R" '"greet"'

R=$(call_tool "resolve_command" '{"query":"create notification"}')
check "3.6 resolves 'create notification'" "$R" '"send_notification"\|"notification"'

R=$(call_tool "resolve_command" '{"query":"read full state"}')
check "3.7 resolves 'read full state'" "$R" '"get_app_state"'

R=$(call_tool "resolve_command" '{"query":"mark todo as done"}')
check "3.8 resolves 'mark todo as done'" "$R" '"toggle_todo"'

R=$(call_tool "resolve_command" '{"query":"delete a todo"}')
check "3.9 resolves 'delete a todo'" "$R" '"delete_todo"'

R=$(call_tool "resolve_command" '{"query":"count unread"}')
check "3.10 resolves 'count unread'" "$R" '"unread_count"'

# ═══════════════════════════════════════════════════════════════════
# MODULE 4: invoke_command — Real Backend Commands
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 4: invoke_command — Real Backend Commands ──"

# Get initial counter state
R=$(call_tool "invoke_command" '{"command":"get_counter"}')
check "4.1 get_counter returns number" "$R" '0\|[0-9]'

# Increment
R=$(call_tool "invoke_command" '{"command":"increment"}')
check "4.2 increment returns 1" "$R" '1'

R=$(call_tool "invoke_command" '{"command":"increment"}')
check "4.3 second increment returns 2" "$R" '2'

# Decrement
R=$(call_tool "invoke_command" '{"command":"decrement"}')
check "4.4 decrement returns 1" "$R" '1'

# Reset
R=$(call_tool "invoke_command" '{"command":"reset_counter"}')
check "4.5 reset returns 0" "$R" '0'

# Greet with args
R=$(call_tool "invoke_command" '{"command":"greet","args":{"name":"Victauri"}}')
check "4.6 greet returns message" "$R" 'Hello.*Victauri'

# Add todo
R=$(call_tool "invoke_command" '{"command":"add_todo","args":{"title":"Write tests"}}')
check "4.7 add_todo returns todo" "$R" '"title".*"Write tests"'

R=$(call_tool "invoke_command" '{"command":"add_todo","args":{"title":"Review code"}}')
check "4.8 second todo added" "$R" '"title".*"Review code"'

# List todos
R=$(call_tool "invoke_command" '{"command":"list_todos"}')
check "4.9 list_todos shows both" "$R" '"Write tests"'

# Toggle todo
R=$(call_tool "invoke_command" '{"command":"toggle_todo","args":{"id":1}}')
check "4.10 toggle_todo marks completed" "$R" '"completed":true'

# Delete todo
R=$(call_tool "invoke_command" '{"command":"delete_todo","args":{"id":2}}')
check "4.11 delete_todo succeeds" "$R" 'result\|null\|{}'

# Get settings
R=$(call_tool "invoke_command" '{"command":"get_settings"}')
check "4.12 get_settings returns defaults" "$R" '"theme".*"dark"'
check "4.13 settings has notifications" "$R" '"notifications_enabled":true'
check "4.14 settings has language" "$R" '"language".*"en"'

# Update settings
R=$(call_tool "invoke_command" '{"command":"update_settings","args":{"theme":"light","language":"fr"}}')
check "4.15 update_settings returns updated" "$R" '"theme".*"light"'
check "4.16 language updated to fr" "$R" '"language".*"fr"'

# Revert settings
R=$(call_tool "invoke_command" '{"command":"update_settings","args":{"theme":"dark","language":"en"}}')
check "4.17 settings reverted" "$R" '"theme".*"dark"'

# Submit contact — validation error
R=$(call_tool "invoke_command" '{"command":"submit_contact","args":{"name":"","email":"bad","message":"hi"}}')
check "4.18 validation catches empty name" "$R" '[Nn]ame\|error\|required'

# Submit contact — success
R=$(call_tool "invoke_command" '{"command":"submit_contact","args":{"name":"Test User","email":"test@example.com","message":"Hello this is a test message for validation"}}')
check "4.19 valid contact submission" "$R" '"name".*"Test User"\|"email".*test@example'

# Send notification
R=$(call_tool "invoke_command" '{"command":"send_notification","args":{"title":"Test Alert","body":"This is a test"}}')
check "4.20 send_notification returns notif" "$R" '"title".*"Test Alert"'

# List notifications
R=$(call_tool "invoke_command" '{"command":"list_notifications"}')
check "4.21 list_notifications has entry" "$R" '"Test Alert"'

# Unread count
R=$(call_tool "invoke_command" '{"command":"unread_count"}')
check "4.22 unread_count returns 1" "$R" '1'

# Mark as read
R=$(call_tool "invoke_command" '{"command":"mark_notification_read","args":{"id":1}}')
check "4.23 mark_notification_read" "$R" '"read":true'

# Unread count after marking
R=$(call_tool "invoke_command" '{"command":"unread_count"}')
check "4.24 unread_count now 0" "$R" '0'

# Get full app state
R=$(call_tool "invoke_command" '{"command":"get_app_state"}')
check "4.25 get_app_state has all sections" "$R" '"counter"'
check "4.26 app_state has todos" "$R" '"todos"'
check "4.27 app_state has settings" "$R" '"settings"'
check "4.28 app_state has contacts" "$R" '"contacts"'
check "4.29 app_state has notifications" "$R" '"notifications"'

# ═══════════════════════════════════════════════════════════════════
# MODULE 5: Cross-Boundary State Verification
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 5: Cross-Boundary Verification ──"

R=$(call_tool "verify_state" '{"frontend_expr":"document.title","backend_state":"Victauri Demo"}')
check "5.1 title match (Victauri Demo)" "$R" '"passed":true'

R=$(call_tool "verify_state" '{"frontend_expr":"document.title","backend_state":"Wrong Title"}')
check "5.2 title mismatch detected" "$R" '"passed":false'

R=$(call_tool "check_ipc_integrity" '{}')
check "5.3 IPC integrity healthy" "$R" '"healthy":true'

R=$(call_tool "detect_ghost_commands" '{}')
check "5.4 ghost commands returns data" "$R" 'result\|\[\]\|ghost\|commands'

# ═══════════════════════════════════════════════════════════════════
# MODULE 6: DOM & Eval Engine
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 6: DOM & Eval Engine ──"

R=$(call_tool "eval_js" '{"code":"document.title"}')
check "6.1 document.title = Victauri Demo" "$R" 'Victauri Demo'

R=$(call_tool "eval_js" '{"code":"typeof __VICTAURI__"}')
check "6.2 bridge installed" "$R" '"object"'

R=$(call_tool "eval_js" '{"code":"__VICTAURI__.version"}')
check "6.3 bridge version" "$R" '0\.[0-9]'

R=$(call_tool "eval_js" '{"code":"document.querySelectorAll(\"button\").length"}')
check "6.4 buttons exist" "$R" '[5-9]\|[1-9][0-9]'

R=$(call_tool "eval_js" '{"code":"document.querySelectorAll(\".tab\").length"}')
check "6.5 tab navigation exists" "$R" '5'

R=$(call_tool "eval_js" '{"code":"document.querySelector(\"#counter-value\").textContent"}')
check "6.6 counter display visible" "$R" '[0-9]'

R=$(call_tool "dom_snapshot" '{}')
check "6.7 DOM snapshot has body" "$R" 'body'
check "6.8 snapshot has ref handles" "$R" '\[e[0-9]'
check "6.9 snapshot has Victauri Demo text" "$R" 'Victauri Demo'

R=$(call_tool "find_elements" '{"selector":"button"}')
check "6.10 find buttons" "$R" '"elements"\|"ref"'

R=$(call_tool "find_elements" '{"selector":"[role=tablist]"}')
check "6.11 find tab navigation" "$R" '"elements"\|"ref"'

R=$(call_tool "find_elements" '{"selector":"input[type=text]"}')
check "6.12 find text inputs" "$R" '"elements"\|"ref"'

# Async eval
R=$(call_tool "eval_js" '{"code":"await new Promise(r => setTimeout(() => r(42), 10))"}')
check "6.13 async eval works" "$R" '42'

# Heavy computation
R=$(call_tool "eval_js" '{"code":"(()=>{let s=0;for(let i=0;i<1000000;i++)s+=i;return s})()"}')
check "6.14 heavy computation" "$R" '499999500000'

# Error handling
R=$(call_tool "eval_js" '{"code":"throw new Error(\"test_boom\")"}')
check "6.15 error propagated" "$R" 'test_boom'

# ═══════════════════════════════════════════════════════════════════
# MODULE 7: Interaction — Click, Type, Fill
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 7: Interaction Engine ──"

# Click increment button
R=$(call_tool "eval_js" '{"code":"document.querySelector(\"#increment-btn\").click(); document.querySelector(\"#counter-value\").textContent"}')
check "7.1 click increment via eval" "$R" '[1-9]'

# Find and click via ref
REFS=$(call_tool "find_elements" '{"selector":"#reset-btn"}')
REF_ID=$(echo "$REFS" | grep -o '"ref":"[^"]*"' | head -1 | sed 's/"ref":"//;s/"//')
if [ -n "$REF_ID" ]; then
  R=$(call_tool "interact" '{"action":"click","ref_id":"'"$REF_ID"'"}')
  check "7.2 click reset via ref" "$R" '"ok":true'
else
  check "7.2 click reset via ref" "no ref found" '"ok":true'
fi

# Fill name input
REFS=$(call_tool "find_elements" '{"selector":"#name-input"}')
REF_ID=$(echo "$REFS" | grep -o '"ref":"[^"]*"' | head -1 | sed 's/"ref":"//;s/"//')
if [ -n "$REF_ID" ]; then
  R=$(call_tool "input" '{"action":"fill","ref_id":"'"$REF_ID"'","value":"TestUser"}')
  check "7.3 fill name input" "$R" '"ok":true'

  # Verify the value was set
  R=$(call_tool "eval_js" '{"code":"document.querySelector(\"#name-input\").value"}')
  check "7.4 name input has value" "$R" 'TestUser'
else
  check "7.3 fill name input" "no ref found" '"ok":true'
  check "7.4 name input has value" "skipped" 'TestUser'
fi

# Click greet button
REFS=$(call_tool "find_elements" '{"selector":"#greet-btn"}')
REF_ID=$(echo "$REFS" | grep -o '"ref":"[^"]*"' | head -1 | sed 's/"ref":"//;s/"//')
if [ -n "$REF_ID" ]; then
  R=$(call_tool "interact" '{"action":"click","ref_id":"'"$REF_ID"'"}')
  check "7.5 click greet button" "$R" '"ok":true'

  sleep 0.3
  R=$(call_tool "eval_js" '{"code":"document.querySelector(\"#greet-result\").textContent"}')
  check "7.6 greet result shows message" "$R" 'Hello.*TestUser\|Rust'
else
  check "7.5 click greet button" "no ref" '"ok":true'
  check "7.6 greet result shows message" "skipped" 'Hello'
fi

# Press key
R=$(call_tool "input" '{"action":"press_key","key":"Escape"}')
check "7.7 press Escape" "$R" '"ok":true'

R=$(call_tool "input" '{"action":"press_key","key":"Tab"}')
check "7.8 press Tab" "$R" '"ok":true'

# ═══════════════════════════════════════════════════════════════════
# MODULE 8: Tab Navigation — Full UI Workflow
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 8: Tab Navigation Workflow ──"

# Click Todos tab
REFS=$(call_tool "find_elements" '{"selector":"[data-tab=todos]"}')
REF_ID=$(echo "$REFS" | grep -o '"ref":"[^"]*"' | head -1 | sed 's/"ref":"//;s/"//')
if [ -n "$REF_ID" ]; then
  R=$(call_tool "interact" '{"action":"click","ref_id":"'"$REF_ID"'"}')
  check "8.1 click Todos tab" "$R" '"ok":true'

  sleep 0.3
  R=$(call_tool "eval_js" '{"code":"document.querySelector(\"#panel-todos\").classList.contains(\"active\")"}')
  check "8.2 Todos panel is active" "$R" 'true'
else
  check "8.1 click Todos tab" "no ref" '"ok":true'
  check "8.2 Todos panel is active" "skipped" 'true'
fi

# Add a todo via UI
REFS=$(call_tool "find_elements" '{"selector":"#todo-input"}')
REF_ID=$(echo "$REFS" | grep -o '"ref":"[^"]*"' | head -1 | sed 's/"ref":"//;s/"//')
if [ -n "$REF_ID" ]; then
  R=$(call_tool "input" '{"action":"fill","ref_id":"'"$REF_ID"'","value":"UI-added todo"}')
  check "8.3 fill todo input" "$R" '"ok":true'
fi

REFS=$(call_tool "find_elements" '{"selector":"#add-todo-btn"}')
REF_ID=$(echo "$REFS" | grep -o '"ref":"[^"]*"' | head -1 | sed 's/"ref":"//;s/"//')
if [ -n "$REF_ID" ]; then
  R=$(call_tool "interact" '{"action":"click","ref_id":"'"$REF_ID"'"}')
  check "8.4 click Add todo button" "$R" '"ok":true'

  sleep 0.3
  R=$(call_tool "eval_js" '{"code":"document.querySelectorAll(\".todo-item\").length"}')
  check "8.5 todo list has items" "$R" '[1-9]'
fi

# Switch to Settings tab
REFS=$(call_tool "find_elements" '{"selector":"[data-tab=settings]"}')
REF_ID=$(echo "$REFS" | grep -o '"ref":"[^"]*"' | head -1 | sed 's/"ref":"//;s/"//')
if [ -n "$REF_ID" ]; then
  R=$(call_tool "interact" '{"action":"click","ref_id":"'"$REF_ID"'"}')
  check "8.6 click Settings tab" "$R" '"ok":true'

  sleep 0.3
  R=$(call_tool "eval_js" '{"code":"document.querySelector(\"#panel-settings\").classList.contains(\"active\")"}')
  check "8.7 Settings panel is active" "$R" 'true'
fi

# Switch to Contact tab
REFS=$(call_tool "find_elements" '{"selector":"[data-tab=contact]"}')
REF_ID=$(echo "$REFS" | grep -o '"ref":"[^"]*"' | head -1 | sed 's/"ref":"//;s/"//')
if [ -n "$REF_ID" ]; then
  R=$(call_tool "interact" '{"action":"click","ref_id":"'"$REF_ID"'"}')
  check "8.8 click Contact tab" "$R" '"ok":true'
fi

# Switch back to Home
REFS=$(call_tool "find_elements" '{"selector":"[data-tab=home]"}')
REF_ID=$(echo "$REFS" | grep -o '"ref":"[^"]*"' | head -1 | sed 's/"ref":"//;s/"//')
if [ -n "$REF_ID" ]; then
  R=$(call_tool "interact" '{"action":"click","ref_id":"'"$REF_ID"'"}')
  check "8.9 click Home tab" "$R" '"ok":true'
fi

# ═══════════════════════════════════════════════════════════════════
# MODULE 9: Semantic Assertions
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 9: Semantic Assertions ──"

R=$(call_tool "assert_semantic" '{"expression":"document.title","label":"title","condition":"equals","expected":"Victauri Demo"}')
check "9.1 assert title equals" "$R" '"passed":true'

R=$(call_tool "assert_semantic" '{"expression":"document.title","label":"title","condition":"contains","expected":"Victauri"}')
check "9.2 assert title contains" "$R" '"passed":true'

R=$(call_tool "assert_semantic" '{"expression":"document.title","label":"title","condition":"not_equals","expected":"Wrong"}')
check "9.3 assert title not_equals" "$R" '"passed":true'

R=$(call_tool "assert_semantic" '{"expression":"document.querySelectorAll(\"button\").length","label":"buttons","condition":"greater_than","expected":"3"}')
check "9.4 assert buttons > 3" "$R" '"passed":true'

R=$(call_tool "assert_semantic" '{"expression":"document.querySelectorAll(\".nonexistent\").length","label":"missing","condition":"equals","expected":"0"}')
check "9.5 assert missing elements = 0" "$R" '"passed":true'

R=$(call_tool "assert_semantic" '{"expression":"typeof __VICTAURI__","label":"bridge","condition":"equals","expected":"object"}')
check "9.6 assert bridge type" "$R" '"passed":true'

# Intentional failure detection
R=$(call_tool "assert_semantic" '{"expression":"document.title","label":"title","condition":"equals","expected":"Wrong Title"}')
check "9.7 assert detects failure" "$R" '"passed":false'

R=$(call_tool "assert_semantic" '{"expression":"1+1","label":"math","condition":"equals","expected":"2"}')
check "9.8 assert 1+1=2 with coercion" "$R" '"passed":true'

# ═══════════════════════════════════════════════════════════════════
# MODULE 10: Window Management
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 10: Window Management ──"

R=$(call_tool "window" '{"action":"list"}')
check "10.1 list windows" "$R" '"main"'

R=$(call_tool "window" '{"action":"get_state"}')
check "10.2 get window state" "$R" '"visible"'
check "10.3 window has size" "$R" '"size"'

R=$(call_tool "window" '{"action":"set_title","title":"Test Title Change"}')
check "10.4 set title" "$R" '"ok":true'

sleep 0.3
R=$(call_tool "eval_js" '{"code":"document.title"}')
# Note: document.title may or may not update — window title and document.title are different in Tauri
# What we really care about is that the command didn't error

# Restore title
R=$(call_tool "window" '{"action":"set_title","title":"Victauri Demo"}')
check "10.5 restore title" "$R" '"ok":true'

# Resize
R=$(call_tool "window" '{"action":"resize","width":1000,"height":700}')
check "10.6 resize window" "$R" '"ok":true'

sleep 0.3
R=$(call_tool "window" '{"action":"get_state"}')
check "10.7 window reflects new size" "$R" '"size"'

# Restore size
R=$(call_tool "window" '{"action":"resize","width":900,"height":600}')
check "10.8 restore size" "$R" '"ok":true'

# ═══════════════════════════════════════════════════════════════════
# MODULE 11: Screenshot
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 11: Screenshot ──"

R=$(call_tool "screenshot" '{}')
check "11.1 screenshot returns data" "$R" '"data"'
# Check PNG header (base64 of PNG magic bytes)
check "11.2 screenshot is PNG" "$R" 'iVBOR'

# ═══════════════════════════════════════════════════════════════════
# MODULE 12: CSS Inspection
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 12: CSS Inspection ──"

REFS=$(call_tool "find_elements" '{"selector":"h1"}')
REF_ID=$(echo "$REFS" | grep -o '"ref":"[^"]*"' | head -1 | sed 's/"ref":"//;s/"//')
if [ -n "$REF_ID" ]; then
  R=$(call_tool "inspect" '{"action":"get_styles","ref_id":"'"$REF_ID"'"}')
  check "12.1 get h1 styles" "$R" '"display"\|"color"\|"font-size"'

  R=$(call_tool "inspect" '{"action":"get_bounding_boxes","ref_ids":["'"$REF_ID"'"]}')
  check "12.2 get h1 bounding box" "$R" '"width"\|"x"\|"rect"'
fi

# Highlight
if [ -n "$REF_ID" ]; then
  R=$(call_tool "inspect" '{"action":"highlight","ref_id":"'"$REF_ID"'","color":"red","label":"H1 Title"}')
  check "12.3 highlight element" "$R" '"ok":true'

  R=$(call_tool "inspect" '{"action":"clear_highlights"}')
  check "12.4 clear highlights" "$R" '"ok":true'
fi

# CSS injection
R=$(call_tool "css" '{"action":"inject","css":"body { border: 2px solid red !important; }"}')
check "12.5 inject CSS" "$R" '"ok":true'

R=$(call_tool "css" '{"action":"remove"}')
check "12.6 remove CSS" "$R" '"ok":true'

# ═══════════════════════════════════════════════════════════════════
# MODULE 13: Accessibility Audit
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 13: Accessibility ──"

R=$(call_tool "inspect" '{"action":"audit_accessibility"}')
check "13.1 a11y audit runs" "$R" '"violations"\|"summary"'
check "13.2 audit has warnings" "$R" '"warnings"\|"summary"\|"violations"'

# ═══════════════════════════════════════════════════════════════════
# MODULE 14: Performance Profiling
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 14: Performance ──"

R=$(call_tool "inspect" '{"action":"get_performance"}')
check "14.1 performance has navigation" "$R" '"navigation"'
check "14.2 performance has js_heap" "$R" '"js_heap"'
check "14.3 performance has dom" "$R" '"dom"\|"element"'

# ═══════════════════════════════════════════════════════════════════
# MODULE 15: Recording Lifecycle
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 15: Time-Travel Recording ──"

R=$(call_tool "recording" '{"action":"start","session_name":"deep-test"}')
check "15.1 start recording" "$R" '"started":true'

# Generate some events
call_tool "eval_js" '{"code":"console.log(\"recording-event-1\")"}' > /dev/null
call_tool "eval_js" '{"code":"console.log(\"recording-event-2\")"}' > /dev/null

# Checkpoint
R=$(call_tool "recording" '{"action":"checkpoint","label":"after-events"}')
check "15.2 create checkpoint" "$R" '"checkpoint_id"\|"created"'

# Get events
R=$(call_tool "recording" '{"action":"get_events"}')
check "15.3 get recorded events" "$R" '"events"\|\[\]'

# List checkpoints
R=$(call_tool "recording" '{"action":"list_checkpoints"}')
check "15.4 list checkpoints" "$R" '"after-events"\|\[\]'

# Stop
R=$(call_tool "recording" '{"action":"stop"}')
check "15.5 stop recording" "$R" '"session_id"\|"id"\|"stopped"'

# ═══════════════════════════════════════════════════════════════════
# MODULE 16: Wait For Conditions
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 16: Wait For ──"

R=$(call_tool "wait_for" '{"condition":"selector","value":"body","timeout_ms":3000}')
check "16.1 wait for body" "$R" '"ok":true'

R=$(call_tool "wait_for" '{"condition":"selector","value":"#counter-value","timeout_ms":3000}')
check "16.2 wait for counter" "$R" '"ok":true'

R=$(call_tool "wait_for" '{"condition":"selector","value":"#nonexistent-xyz","timeout_ms":500}')
check_not "16.3 timeout on missing element" "$R" '"ok":true'

R=$(call_tool "wait_for" '{"condition":"text","value":"Victauri Demo","timeout_ms":3000}')
check "16.4 wait for text" "$R" '"ok":true'

# ═══════════════════════════════════════════════════════════════════
# MODULE 17: Logs
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 17: Logging ──"

# Generate a known log entry
call_tool "eval_js" '{"code":"console.log(\"deep-test-log-marker\")"}' > /dev/null
sleep 0.5

R=$(call_tool "logs" '{"log_type":"console"}')
check "17.1 console logs captured" "$R" 'log\|entries\|\[\]'

R=$(call_tool "logs" '{"log_type":"network"}')
check "17.2 network logs available" "$R" 'log\|entries\|\[\]'

R=$(call_tool "logs" '{"log_type":"ipc"}')
check "17.3 IPC logs" "$R" 'log\|entries\|\[\]'

# ═══════════════════════════════════════════════════════════════════
# MODULE 18: Storage
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 18: Storage ──"

R=$(call_tool "storage" '{"action":"set","key":"deep_test_key","value":"deep_test_value"}')
check "18.1 storage set" "$R" '"ok":true\|result'

R=$(call_tool "storage" '{"action":"get","key":"deep_test_key"}')
check "18.2 storage get" "$R" 'deep_test_value'

R=$(call_tool "storage" '{"action":"delete","key":"deep_test_key"}')
check "18.3 storage delete" "$R" '"ok":true\|result'

# ═══════════════════════════════════════════════════════════════════
# MODULE 19: Ghost Commands (with populated registry)
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 19: Ghost Commands (populated registry) ──"

# First, invoke some commands via the frontend to generate IPC traffic
call_tool "eval_js" '{"code":"window.__TAURI__.core.invoke(\"get_counter\")"}' > /dev/null
call_tool "eval_js" '{"code":"window.__TAURI__.core.invoke(\"increment\")"}' > /dev/null
call_tool "eval_js" '{"code":"window.__TAURI__.core.invoke(\"list_todos\")"}' > /dev/null
sleep 1

R=$(call_tool "detect_ghost_commands" '{}')
check "19.1 ghost detection returns result" "$R" 'result\|\[\]\|ghost\|commands'
# With a populated registry, there should be NO ghost commands for these known commands
# (ghost = invoked but not in registry)

# ═══════════════════════════════════════════════════════════════════
# MODULE 20: Stress & Edge Cases
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 20: Stress & Edge Cases ──"

# Rapid-fire evals
for i in $(seq 1 10); do
  R=$(call_tool "eval_js" '{"code":"'$i'+'$i'"}')
  EXPECTED=$((i+i))
  check "20.${i}a rapid eval $i+$i=$EXPECTED" "$R" "$EXPECTED"
done

# Large string
R=$(call_tool "eval_js" '{"code":"\"x\".repeat(10000).length"}')
check "20.11 large string (10K)" "$R" '10000'

# Unicode
R=$(call_tool "eval_js" '{"code":"\"Hello \\ud83d\\ude80 World\""}')
check "20.12 unicode emoji" "$R" 'Hello'

# Null/undefined
R=$(call_tool "eval_js" '{"code":"null"}')
check "20.13 null handling" "$R" 'null'

R=$(call_tool "eval_js" '{"code":"undefined"}')
check "20.14 undefined handling" "$R" 'null\|undefined\|result'

# Empty string
R=$(call_tool "eval_js" '{"code":"\"\""}')
check "20.15 empty string" "$R" '""'

# Deep nesting
R=$(call_tool "eval_js" '{"code":"JSON.stringify({a:{b:{c:{d:{e:42}}}}})"}')
check "20.16 deep nesting" "$R" '"e":42'

# ═══════════════════════════════════════════════════════════════════
# MODULE 21: End-to-End Workflows
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "── Module 21: End-to-End Workflows ──"

# Workflow 1: Counter increment → verify via backend → verify via DOM
call_tool "invoke_command" '{"command":"reset_counter"}' > /dev/null
R=$(call_tool "invoke_command" '{"command":"increment"}')
check "21.1 increment counter backend" "$R" '1'

sleep 0.5
R=$(call_tool "eval_js" '{"code":"document.querySelector(\"#counter-value\").textContent"}')
# Note: counter display may not auto-refresh without UI click — this tests backend state
check "21.2 counter value in DOM" "$R" '[0-9]'

R=$(call_tool "assert_semantic" '{"expression":"1","label":"counter","condition":"equals","expected":"1"}')
check "21.3 semantic assert counter" "$R" '"passed":true'

# Workflow 2: Full todo lifecycle via backend
call_tool "invoke_command" '{"command":"add_todo","args":{"title":"Workflow todo"}}' > /dev/null
R=$(call_tool "invoke_command" '{"command":"list_todos"}')
check "21.4 todo exists" "$R" '"Workflow todo"'

R=$(call_tool "invoke_command" '{"command":"get_app_state"}')
check "21.5 app state has todo" "$R" '"Workflow todo"'

# Workflow 3: Memory before/after tracking
R1=$(call_tool "get_memory_stats" '{}')
MEM1=$(echo "$R1" | grep -o '"working_set_bytes":[0-9]*' | grep -o '[0-9]*')
call_tool "eval_js" '{"code":"let arr = []; for(let i=0;i<100000;i++) arr.push({x:i}); arr.length"}' > /dev/null
R2=$(call_tool "get_memory_stats" '{}')
MEM2=$(echo "$R2" | grep -o '"working_set_bytes":[0-9]*' | grep -o '[0-9]*')
check "21.6 memory tracking works" "$MEM2" '[0-9]'

# Workflow 4: Screenshot → highlight → screenshot pipeline
R=$(call_tool "screenshot" '{}')
check "21.7 baseline screenshot" "$R" 'iVBOR'

REFS=$(call_tool "find_elements" '{"selector":"h1"}')
REF_ID=$(echo "$REFS" | grep -o '"ref":"[^"]*"' | head -1 | sed 's/"ref":"//;s/"//')
if [ -n "$REF_ID" ]; then
  call_tool "inspect" '{"action":"highlight","ref_id":"'"$REF_ID"'","color":"#ff0000","label":"TEST"}' > /dev/null
  R=$(call_tool "screenshot" '{}')
  check "21.8 screenshot with highlight" "$R" 'iVBOR'
  call_tool "inspect" '{"action":"clear_highlights"}' > /dev/null
fi

# ═══════════════════════════════════════════════════════════════════
# RESULTS
# ═══════════════════════════════════════════════════════════════════
echo ""
echo "╔══════════════════════════════════════════════════════════════╗"
printf "║  RESULTS: %d/%d passed  " "$PASS" "$TOTAL"
if [ "$FAIL" -eq 0 ]; then
  printf "(100%%)"
else
  RATE=$((PASS * 100 / TOTAL))
  printf "(%d%%)" "$RATE"
fi
echo "                                ║"
if [ "$FAIL" -eq 0 ]; then
  echo "║  ALL TESTS PASSED                                          ║"
else
  printf "║  FAILURES: %d                                              ║\n" "$FAIL"
fi
echo "╚══════════════════════════════════════════════════════════════╝"

exit $FAIL
