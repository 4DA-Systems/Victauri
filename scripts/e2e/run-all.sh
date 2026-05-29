#!/bin/bash
# Run all Victauri E2E suites against a running app on :7373.
# Usage: bash scripts/e2e/run-all.sh
set -uo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
BASE="${VICTAURI_BASE:-http://127.0.0.1:7373}"

if ! curl -s -m 3 "$BASE/health" | grep -qi ok; then
  echo "ERROR: Victauri server not reachable at $BASE/health"
  echo "Start your Tauri app with victauri-plugin first."
  exit 1
fi

echo "Victauri E2E — target $BASE"
curl -s -m 3 "$BASE/info" | jq -c '{version, commands_registered, auth_required}' 2>/dev/null || true
echo ""

rc=0
for s in 01-exhaustive-core.sh 02-exhaustive-extended.sh 03-adversarial-limits.sh; do
  echo "════════════════════════════════════════════════"
  echo "  RUNNING $s"
  echo "════════════════════════════════════════════════"
  bash "$DIR/$s" || rc=1
  echo ""
done

echo "All suites complete (exit $rc). Review output above; adversarial results in scripts/adversarial-results.txt."
exit $rc
