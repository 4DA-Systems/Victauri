#!/usr/bin/env bash
# macos-tcc-check.sh — verify the two TCC-gated tools on macOS AFTER a human has
# granted Screen Recording + Accessibility to the terminal/app via System Settings
# (over VNC). Run this separately from macos-deep-test.sh.
#
#   bash scripts/deep-test/macos-tcc-check.sh
#
# Assumes the demo-app is already running on :7373 (or run macos-deep-test.sh first
# in another shell, or launch ./target/debug/demo-app & here).
set -uo pipefail
PORT="${VICTAURI_PORT:-7373}"
B="http://127.0.0.1:${PORT}/api/tools"
call() { curl -sf -X POST "$B/$1" -H 'content-type: application/json' --data-binary "$2" 2>/dev/null; }

echo "=== TCC-gated tool checks (macOS) ==="
curl -sf "http://127.0.0.1:${PORT}/health" >/dev/null 2>&1 || { echo "❌ demo-app not running on :$PORT — start it first"; exit 1; }

# 1. Screenshot — needs Screen Recording permission. Expect a base64 PNG (iVBORw0KGgo…).
echo "[screenshot] needs: Screen Recording"
shot=$(call screenshot '{}')
if echo "$shot" | grep -q 'iVBORw0KGgo'; then
  echo "  ✅ screenshot returned a valid PNG (Screen Recording granted)"
else
  echo "  ⚠️ no PNG — either Screen Recording not granted, or capture returned empty"
  echo "     got: $(echo "$shot" | head -c 160)"
fi

# 2. Trusted input — needs Accessibility. Today macOS native input is STUBBED
#    (falls back to synthetic isTrusted:false). This documents the known gap and
#    will flip to true once CGEvent native input lands + Accessibility is granted.
echo "[trusted input] needs: Accessibility (CGEvent impl pending)"
# Type into the name field with trusted:true, then read back isTrusted of the last key.
call eval_js '{"code":"window.__lastTrusted=null; document.addEventListener(\"keydown\",function(e){window.__lastTrusted=e.isTrusted;},{once:true}); var el=document.querySelector(\"[data-testid=name-input]\"); if (el) el.focus(); return \"armed\""}' >/dev/null
# `input` takes a snapshot `ref_id`, not a CSS selector (and there is no `type` action).
# The field was focused above; `press_key` with no `ref_id` sends the trusted key to the
# focused element.
call input '{"action":"press_key","key":"x","trusted":true}' >/dev/null
sleep 1
it=$(call eval_js '{"code":"return String(window.__lastTrusted)"}')
echo "  isTrusted observed -> $it"
echo "$it" | grep -q true && echo "  ✅ TRUSTED input works on macOS (CGEvent live)" \
                          || echo "  ℹ️ synthetic fallback (expected until macOS CGEvent input is implemented)"

echo "=== done ==="
