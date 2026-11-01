#!/usr/bin/env bash
# Proves check-release-no-cache.sh on the real workflows and on each way a cache could slip in.
# Run by CI (`check`).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
check="$root/scripts/check-release-no-cache.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fail=0

reset() {
  cp "$root/.github/workflows/release.yml" "$tmp/release.yml"
  cp "$root/.github/workflows/release-vscode.yml" "$tmp/vscode.yml"
  cp "$root/.github/actions/linux-deps/action.yml" "$tmp/deps.yml"
}
run_check() { bash "$check" "$tmp/release.yml" "$tmp/vscode.yml" "$tmp/deps.yml" >"$tmp/out" 2>&1; }
expect_pass() {
  if ! run_check; then echo "FAIL (should pass): $1"; cat "$tmp/out"; fail=1; fi
}
expect_refused() {
  if run_check; then echo "FAIL (should be refused): $1"; fail=1; fi
}
# A step with one extra line in its `with:`, appended to the workflow.
add_step() {
  printf '      - uses: %s\n        with:\n          %s\n' "$1" "$2" >>"$tmp/$3"
}

reset; expect_pass "the real workflows"

for value in 'cache: npm' 'cache: "npm"' "cache: 'npm'" 'cache: ${{ matrix.c }}' \
  'package-manager-cache: true' 'package-manager-cache: "true"'; do
  reset; add_step actions/setup-node@v4 "$value" vscode.yml; expect_refused "$value"
done
reset; add_step actions/setup-node@v4 'cache: "false"' vscode.yml; expect_pass 'cache: "false"'
reset; add_step actions/setup-node@v4 'package-manager-cache: false' vscode.yml
expect_pass 'package-manager-cache: false'

# Caches by default, so its very name is the finding.
reset; printf '      - uses: actions-rust-lang/setup-rust-toolchain@v1\n' >>"$tmp/release.yml"
expect_refused "setup-rust-toolchain"
reset; printf '      - uses: Swatinem/rust-cache@v2\n' >>"$tmp/release.yml"; expect_refused "rust-cache"
reset; printf '      # uses: actions/cache@v4, once\n' >>"$tmp/release.yml"; expect_pass "a comment"

# linux-deps: a step with no name after a guarded one must not inherit its guard.
reset
printf '      - uses: actions/cache@v4\n' >>"$tmp/deps.yml"
expect_refused "an unnamed cache step after a guarded one"
reset
printf "      - if: inputs.cache == 'true'\n        uses: actions/cache@v4\n" >>"$tmp/deps.yml"
expect_pass "a guarded step spelled with a leading if"
reset
printf "      - uses: actions/cache@v4\n        if: inputs.cache == 'true'\n" >>"$tmp/deps.yml"
expect_pass "a guarded step with the if after the uses"
reset
printf "      - name: x\n        if: inputs.cache == 'true' || true\n        uses: actions/cache@v4\n" >>"$tmp/deps.yml"
expect_refused "a guard that always passes"

[ "$fail" -eq 0 ] && echo "check-release-no-cache: all fixtures behave"
exit "$fail"
