#!/usr/bin/env bash
# macos-deep-test.sh — one-shot deep test battery for Victauri on a fresh
# macOS (or Linux) host. Designed for a rented cloud Mac (e.g. Scaleway Mac mini)
# reached over SSH. Runs everything that does NOT need TCC/GUI permission grants;
# the two TCC-gated tools (screenshot, trusted input) are checked separately after
# a one-time VNC "Allow" (see macos-tcc-check.sh).
#
# Usage (on the Mac, repo already cloned):
#   bash scripts/deep-test/macos-deep-test.sh
#
# Safe to re-run. Exits non-zero if any hard check fails.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"
# macOS defaults to ulimit -n 256, which the concurrent-server adversarial
# battery exhausts ("Too many open files" — false test failures, not bugs).
# Raise it; harmless where already higher (Linux). Verified: 256 -> 8192 fixed it.
ulimit -n 8192 2>/dev/null || true
PORT="${VICTAURI_PORT:-7373}"
B="http://127.0.0.1:${PORT}/api/tools"
LOG="/tmp/victauri-deep-test"
mkdir -p "$LOG"
PASS=0; FAIL=0
ok()   { echo "  ✅ $1"; PASS=$((PASS+1)); }
bad()  { echo "  ❌ $1"; FAIL=$((FAIL+1)); }
hdr()  { echo; echo "=== $1 ==="; }
call() { curl -sf -X POST "$B/$1" -H 'content-type: application/json' --data-binary "$2" 2>/dev/null; }

echo "################ Victauri macOS/Linux deep-test ################"
echo "host: $(uname -a)"
echo "repo: $REPO_ROOT @ $(git rev-parse --short HEAD 2>/dev/null || echo '?')"

# ── 0. Toolchain ──────────────────────────────────────────────────────────────
hdr "0. Toolchain"
command -v cargo >/dev/null && ok "cargo: $(cargo --version)" || { bad "cargo missing — install rustup first"; exit 1; }
command -v jq    >/dev/null && ok "jq present" || bad "jq missing (brew install jq / apt install jq)"

# ── 1. Build + unit/integration tests ─────────────────────────────────────────
hdr "1. Build workspace + run tests"
if cargo build --workspace > "$LOG/build.log" 2>&1; then ok "workspace builds"; else bad "workspace build failed (see $LOG/build.log)"; tail -20 "$LOG/build.log"; fi
if cargo test --workspace > "$LOG/test.log" 2>&1; then
  ok "workspace tests: $(grep -hoE '[0-9]+ passed' "$LOG/test.log" | awk '{s+=$1} END{print s" passed"}')"
else bad "some workspace tests failed (see $LOG/test.log)"; grep -E 'FAILED|error\[' "$LOG/test.log" | head; fi
if cargo clippy --workspace --all-targets > "$LOG/clippy.log" 2>&1; then ok "clippy clean"; else bad "clippy warnings (see $LOG/clippy.log)"; fi

# ── 2. Launch demo-app + full-stack proof ─────────────────────────────────────
hdr "2. Launch demo-app + full-stack proof (the 5 layers)"
cargo build -p demo-app --bin demo-app >> "$LOG/build.log" 2>&1
BIN=./target/debug/demo-app
case "$(uname -s)" in
  Linux)
    export WEBKIT_DISABLE_DMABUF_RENDERER=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1
    if command -v xvfb-run >/dev/null; then xvfb-run -a --server-args="-screen 0 1280x800x24" "$BIN" > "$LOG/app.log" 2>&1 &
    else "$BIN" > "$LOG/app.log" 2>&1 & fi ;;
  *) "$BIN" > "$LOG/app.log" 2>&1 & ;;
esac
APP_PID=$!
up=false
for i in $(seq 1 90); do curl -sf "http://127.0.0.1:${PORT}/health" >/dev/null 2>&1 && { up=true; break; }; kill -0 "$APP_PID" 2>/dev/null || break; sleep 1; done
if [ "$up" = true ]; then ok "MCP server up on :$PORT"; else bad "MCP server never came up"; cat "$LOG/app.log"; fi

if [ "$up" = true ]; then
  wv=false; for i in $(seq 1 45); do r=$(call eval_js '{"code":"return 6*7"}'); echo "$r" | grep -q 42 && { wv=true; break; }; sleep 2; done
  [ "$wv" = true ] && ok "[webview] eval 6*7 -> $r" || bad "[webview] eval not ready ($r)"
  d=$(call dom_snapshot '{}');        echo "$d"  | grep -q ref    && ok "[dom] snapshot ok"        || bad "[dom] snapshot"
  i=$(call invoke_command '{"command":"get_counter","args":{}}'); echo "$i" | grep -q result && ok "[ipc->rust] get_counter -> $i" || bad "[ipc->rust] invoke"
  n=$(call get_registry '{}' | jq '.result | length'); [ "${n:-0}" -ge 1 ] && ok "[backend] registry: $n cmds" || bad "[backend] registry"
  m=$(call get_memory_stats '{}');    echo "$m" | grep -qiE 'bytes|resident' && ok "[native] memory -> $m" || bad "[native] memory"
fi

# ── 3. query_db on a real seeded SQLite (the DB-layer moat, on macOS) ──────────
hdr "3. query_db on a seeded SQLite DB"
DB="$LOG/seed.db"
if command -v sqlite3 >/dev/null; then
  rm -f "$DB"
  sqlite3 "$DB" "CREATE TABLE metrics(id INTEGER PRIMARY KEY, name TEXT, val REAL); INSERT INTO metrics(name,val) VALUES('accuracy',0.174),('rows',125610);" 2>/dev/null
  q=$(call query_db "{\"query\":\"SELECT name,val FROM metrics ORDER BY id\",\"path\":\"$DB\"}")
  if echo "$q" | grep -q 125610; then ok "[db] read seeded DB via query_db: $q"; else bad "[db] query_db (may need db_search_paths config for this path): $q"; fi
else bad "sqlite3 not installed — skip DB seed (brew install sqlite)"; fi

# ── 4. Adversarial E2E suite (the no-happy-path regression battery) ────────────
hdr "4. Adversarial E2E suite"
if VICTAURI_E2E=1 cargo test -p demo-app --test adversarial -- --test-threads=1 > "$LOG/adversarial.log" 2>&1; then
  ok "adversarial suite passed ($(grep -hoE '[0-9]+ passed' "$LOG/adversarial.log" | tail -1))"
else bad "adversarial suite had failures (see $LOG/adversarial.log)"; grep -E 'FAILED|panicked' "$LOG/adversarial.log" | head; fi

# ── 5. Soak: 200 rapid evals must all succeed (stability) ──────────────────────
hdr "5. Soak — 200 rapid evals"
soakfail=0
for i in $(seq 1 200); do r=$(call eval_js '{"code":"return 1+1"}'); echo "$r" | grep -q 2 || soakfail=$((soakfail+1)); done
[ "$soakfail" = 0 ] && ok "200/200 evals ok" || bad "$soakfail/200 evals failed"

kill "$APP_PID" 2>/dev/null || true

# ── Summary ───────────────────────────────────────────────────────────────────
hdr "SUMMARY"
echo "PASS=$PASS  FAIL=$FAIL   (logs in $LOG)"
echo "NOTE: TCC-gated tools (screenshot, trusted input) are NOT covered here —"
echo "run scripts/deep-test/macos-tcc-check.sh after granting Screen Recording +"
echo "Accessibility in System Settings via VNC."
[ "$FAIL" = 0 ] && echo "################ ALL DEEP CHECKS GREEN ################" || echo "################ $FAIL CHECK(S) FAILED ################"
exit "$FAIL"
