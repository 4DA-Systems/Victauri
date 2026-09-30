#!/usr/bin/env bash
# Doc-count lint — keeps the headline numbers in user-facing docs in sync with the
# code, so they can never silently drift again (they had: "31"/"33"/"34 tools"
# scattered across the docs).
#
# Source of truth is always the code. This checks the canonical, unambiguous claim
# strings in the docs (it deliberately does NOT touch CLAUDE.md, which is a
# historical changelog where old counts are expected).
#
# Run locally (`scripts/check-doc-counts.sh`) or in CI. Exits non-zero on mismatch.
set -euo pipefail
cd "$(dirname "$0")/.."

status=0

expect() { # expect <description> <actual> <claimed> <file>
  local desc="$1" actual="$2" claimed="$3" file="$4"
  if [ -z "$claimed" ]; then
    echo "::error file=$file::doc-count lint could not find the '$desc' claim (did the wording change?)"
    status=1
  elif [ "$claimed" != "$actual" ]; then
    echo "::error file=$file::$desc claims $claimed but code has $actual"
    status=1
  else
    echo "  ok: $desc = $actual"
  fi
}

# Counts matches of an extended regex, tolerating zero matches. grep exits 1 on
# no match, which under `set -euo pipefail` would hard-exit the script before the
# lint can report the mismatch as a proper failure — so swallow that exit and
# return 0 instead.
count() {
  local pattern="$1"
  shift
  { grep -rhoE "$pattern" "$@" || true; } | wc -l | tr -d ' '
}

# ── MCP tool count ───────────────────────────────────────────────────────────
tools=$(count '#\[tool\(' crates/victauri-plugin/src/mcp/)
echo "MCP tools defined in code: $tools"
expect "README 'N tools across the full stack'" "$tools" \
  "$(grep -oE '[0-9]+ tools across the full stack' README.md | grep -oE '^[0-9]+' || true)" \
  README.md
expect "README 'All N tools'" "$tools" \
  "$(grep -oE 'All [0-9]+ tools' README.md | grep -oE '[0-9]+' || true)" \
  README.md
expect "getting-started 'All N tools ... REST'" "$tools" \
  "$(grep -oE 'All [0-9]+ tools are also available' docs/src/getting-started.md | grep -oE '[0-9]+' || true)" \
  docs/src/getting-started.md
expect "getting-started 'Complete list of all N tools'" "$tools" \
  "$(grep -oE 'Complete list of all [0-9]+ tools' docs/src/getting-started.md | grep -oE '[0-9]+' | head -1 || true)" \
  docs/src/getting-started.md
expect "tools-reference 'exposes N MCP tools'" "$tools" \
  "$(grep -oE 'exposes [0-9]+ MCP tools' docs/src/tools-reference.md | grep -oE '[0-9]+' || true)" \
  docs/src/tools-reference.md

expect "plugin README 'N MCP tools across three layers'" "$tools" \
  "$(grep -oE '[0-9]+ MCP tools across three layers' crates/victauri-plugin/README.md | grep -oE '^[0-9]+' || true)" \
  crates/victauri-plugin/README.md

# ── Smoke-suite check count (`victauri test`, `VictauriClient::smoke_test`) ───
checks=$(count '(skippable_)?check!\(' crates/victauri-test/src/smoke.rs)
echo "Smoke checks defined in code: $checks"
expect "README 'all N smoke checks'" "$checks" \
  "$(grep -oE 'all [0-9]+ smoke checks' README.md | grep -oE '[0-9]+' || true)" \
  README.md
expect "CLI README '(N checks)'" "$checks" \
  "$(grep -oE 'smoke test suite \([0-9]+ checks\)' crates/victauri-cli/README.md | grep -oE '[0-9]+' || true)" \
  crates/victauri-cli/README.md
expect "victauri-test README 'Runs N checks'" "$checks" \
  "$(grep -oE 'Runs [0-9]+ checks' crates/victauri-test/README.md | grep -oE '[0-9]+' || true)" \
  crates/victauri-test/README.md
expect "testing guide 'The N checks'" "$checks" \
  "$(grep -oE 'The [0-9]+ checks' docs/src/testing.md | grep -oE '[0-9]+' || true)" \
  docs/src/testing.md
expect "testing-tauri-apps 'CLI — N checks'" "$checks" \
  "$(grep -oE 'CLI — [0-9]+ checks' docs/src/testing-tauri-apps.md | grep -oE '[0-9]+' || true)" \
  docs/src/testing-tauri-apps.md
expect "testing-tauri-apps 'victauri test (N checks'" "$checks" \
  "$(grep -oE 'victauri test \([0-9]+ checks' docs/src/testing-tauri-apps.md | grep -oE '[0-9]+' || true)" \
  docs/src/testing-tauri-apps.md

if [ "$status" -ne 0 ]; then
  echo
  echo "Doc-count lint FAILED — update the doc number(s) above to match the code."
  exit 1
fi
echo
echo "Doc-count lint passed."
