#!/usr/bin/env bash
# Doc-count lint — keeps the headline numbers in user-facing docs in sync with the
# code, so they can never silently drift again (they had: "31"/"33"/"34 tools" and
# "163"/"169 vitest tests" scattered across the docs).
#
# Source of truth is always the code. This checks the canonical, unambiguous claim
# strings in README.md (it deliberately does NOT touch the "20 MCP tools" browser
# line — that count is the extension's, not the plugin's — nor CLAUDE.md, which is a
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

# ── MCP tool count ───────────────────────────────────────────────────────────
tools=$(grep -rohE '#\[tool\(' crates/victauri-plugin/src/mcp/ | wc -l | tr -d ' ')
echo "MCP tools defined in code: $tools"
expect "README 'N tools across the full stack'" "$tools" \
  "$(grep -oE '[0-9]+ tools across the full stack' README.md | grep -oE '^[0-9]+' || true)" \
  README.md
expect "README 'All N tools'" "$tools" \
  "$(grep -oE 'All [0-9]+ tools' README.md | grep -oE '[0-9]+' || true)" \
  README.md

# ── Chrome extension vitest count ────────────────────────────────────────────
vitest=$(grep -rhoE '^[[:space:]]*(it|test)\(' extensions/chrome/tests/*.test.js | wc -l | tr -d ' ')
echo "Chrome vitest tests defined: $vitest"
expect "README 'N vitest tests'" "$vitest" \
  "$(grep -oE '[0-9]+ vitest tests' README.md | grep -oE '^[0-9]+' || true)" \
  README.md

if [ "$status" -ne 0 ]; then
  echo
  echo "Doc-count lint FAILED — update the doc number(s) above to match the code."
  exit 1
fi
echo
echo "Doc-count lint passed."
