#!/usr/bin/env bash
# Local pre-publish gate (see preflight.ps1 for the rationale). Runs everything checkable on
# a dev machine BEFORE pushing a release commit. It does NOT replace CI: cross-platform bugs
# (macOS/Linux) and the real-app E2E are verified by CI, and the release workflow refuses to
# publish unless the full CI is green for the commit (the `require-ci-green` job).
set -u
cd "$(dirname "$0")/.."

failed=()
run() {
  local name="$1"; shift
  echo; echo "=== $name ==="
  if "$@"; then echo "ok: $name"; else echo "FAILED: $name"; failed+=("$name"); fi
}

run "cargo fmt --check" cargo fmt --all -- --check
run "clippy"            cargo clippy --workspace --all-targets --features sqlite -- -D warnings
run "doc-count lint"    bash scripts/check-doc-counts.sh
run "workspace tests"   cargo test --workspace --features sqlite

echo
if [ "${#failed[@]}" -gt 0 ]; then
  echo "PREFLIGHT FAILED: ${failed[*]}"
  echo "Fix these before pushing. (macOS/Linux + the real-app E2E are still verified by CI.)"
  exit 1
fi
echo "PREFLIGHT PASSED (local gate)."
echo "Next: push, wait for CI GREEN on all platforms, then bump + tag. The release workflow"
echo "will refuse to publish unless CI is green for that commit."
