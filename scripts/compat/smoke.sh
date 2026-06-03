#!/usr/bin/env bash
# App-agnostic Victauri compatibility smoke battery.
#
# Given the base URL of a running Victauri server (REST API), exercises the
# tools that work on ANY Tauri app — no app-specific commands — and prints a
# pass/fail line per check plus a one-line JSON summary on the last line:
#
#   {"checks":N,"passed":P,"failed":F}
#
# Usage: smoke.sh [base_url]   (default http://127.0.0.1:7373)
#
# Exit code is the number of failed checks (0 = all passed), capped at 125.
set -uo pipefail

BASE="${1:-http://127.0.0.1:7373}"
T="$BASE/api/tools"

passed=0
failed=0

# check <name> <tool> <json-body> <grep-pattern>
# Passes when the REST response body matches <grep-pattern> (extended regex).
check() {
  local name="$1" tool="$2" body="$3" pat="$4"
  local resp
  resp=$(curl -sf -X POST "$T/$tool" -H 'content-type: application/json' --data-binary "$body" 2>/dev/null || true)
  if printf '%s' "$resp" | grep -qE "$pat"; then
    echo "  PASS  $name"
    passed=$((passed + 1))
  else
    echo "  FAIL  $name   (got: $(printf '%s' "$resp" | head -c 120))"
    failed=$((failed + 1))
  fi
}

echo "=== Victauri compatibility smoke battery @ $BASE ==="

# Server reachable + identifies itself.
if curl -sf "$BASE/health" >/dev/null 2>&1; then
  echo "  PASS  health endpoint"; passed=$((passed + 1))
else
  echo "  FAIL  health endpoint"; failed=$((failed + 1))
fi

# Webview eval — arithmetic and a document property.
check "eval_js arithmetic"      eval_js     '{"code":"return 6*7"}'                                  '(^|[^0-9])42([^0-9]|$)'
check "eval_js document.title"  eval_js     '{"code":"return document.title || \"untitled\""}'       '"result"'
check "eval_js DOM access"      eval_js     '{"code":"return document.querySelectorAll(\"*\").length"}' '"result":[1-9]'

# Accessible DOM snapshot with ref handles.
check "dom_snapshot has refs"   dom_snapshot '{}'                                                     '\[e[0-9]+\]|"ref"'

# Element finding (every app has at least one element).
check "find_elements body"     find_elements '{"css":"body"}'                                        '"result"'

# Native / backend introspection (no web API can do these).
check "get_memory_stats"       get_memory_stats '{}'                                                 'bytes|resident|working_set'
check "window list"            window      '{"action":"list"}'                                       '\["'
check "get_diagnostics"        get_diagnostics '{}'                                                  '"result"'
check "get_plugin_info"        get_plugin_info '{}'                                                  'port|version'

# Deep introspection tools.
check "inspect a11y audit"     inspect     '{"action":"audit_accessibility"}'                        '"result"'
check "inspect performance"    inspect     '{"action":"get_performance"}'                            '"result"'
check "logs console"           logs        '{"action":"console"}'                                    '"result"'

# Storage round-trip (localStorage is available in any webview).
check "storage set"            storage     '{"action":"set","key":"__vic_compat","value":"ok"}'     '"result"|true'
check "storage get"            storage     '{"action":"get","key":"__vic_compat"}'                  'ok'

total=$((passed + failed))
echo "=== ${passed}/${total} passed ==="
# Machine-readable summary on the final line.
echo "{\"checks\":$total,\"passed\":$passed,\"failed\":$failed}"

[ "$failed" -gt 125 ] && exit 125
exit "$failed"
