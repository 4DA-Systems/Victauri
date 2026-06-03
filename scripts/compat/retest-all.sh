#!/usr/bin/env bash
# Run the compatibility retest for every app in apps.json and print a Markdown
# summary table. Individual app failures do not abort the run — the goal is a
# full matrix of results against the current Victauri.
#
# Usage: scripts/compat/retest-all.sh
# Exit code: number of apps that did not pass cleanly (0 = all green).
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
keys=$(jq -r '.apps[].key' "$here/apps.json")

declare -a rows
not_green=0

for k in $keys; do
  echo "######## $k ########"
  out=$(bash "$here/retest-app.sh" "$k" 2>&1)
  echo "$out"
  json=$(printf '%s\n' "$out" | grep -E '^\{"app"' | tail -1)
  name=$(jq -r '.name'   <<<"$json" 2>/dev/null || echo "$k")
  stage=$(jq -r '.stage' <<<"$json" 2>/dev/null || echo "?")
  checks=$(jq -r '.checks' <<<"$json" 2>/dev/null || echo 0)
  passed=$(jq -r '.passed' <<<"$json" 2>/dev/null || echo 0)
  failed=$(jq -r '.failed' <<<"$json" 2>/dev/null || echo 0)
  if [ "${checks:-0}" -gt 0 ] && [ "${failed:-1}" -eq 0 ]; then
    result="✅ ${passed}/${checks}"
  elif [ "${checks:-0}" -gt 0 ]; then
    result="⚠️ ${passed}/${checks}"; not_green=$((not_green + 1))
  else
    result="❌ failed at *${stage}*"; not_green=$((not_green + 1))
  fi
  rows+=("| $name | $result |")
done

echo
echo "## Victauri compatibility retest"
echo
echo "Victauri $(grep -m1 '^version' "$here/../../Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/') — $(date -u +%Y-%m-%d)"
echo
echo "| App | Smoke battery |"
echo "|-----|---------------|"
printf '%s\n' "${rows[@]}"

exit "$not_green"
