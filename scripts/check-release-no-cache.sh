#!/usr/bin/env bash
# Fails if a release workflow can restore a cache. A tag run reads main's cache scope, and
# compat.yml runs third-party build scripts in that scope, so a restored entry could feed the
# binaries release.yml ships. Run by CI (`check`). Comments are ignored, so a note saying why
# there is no cache stays allowed.
#
# Usage: check-release-no-cache.sh [release.yml] [release-vscode.yml] [linux-deps/action.yml]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
release="${1:-$root/.github/workflows/release.yml}"
vscode="${2:-$root/.github/workflows/release-vscode.yml}"
deps="${3:-$root/.github/actions/linux-deps/action.yml}"
fail=0

# A `cache` key, quoted or not, up to its value.
cache_key="[\"']?[A-Za-z-]*[Cc][Aa][Cc][Hh][Ee][\"']?[[:space:]]*:[[:space:]]*"
for f in "$release" "$vscode"; do
  [ -s "$f" ] || { echo "ERROR: $f is missing or empty"; exit 1; }
  # Also `cache` keys, which is how setup-node and friends switch a cache on: on their own line or
  # in a `{ flow, mapping }`, anything but false. So each pair saying false is dropped first, and
  # any key left is a finding. setup-rust-toolchain caches unless told otherwise, so naming it is
  # enough.
  hits="$(sed -E -e 's/(^|[[:space:]])#.*$//' \
    -e "s/${cache_key}[\"']?[Ff][Aa][Ll][Ss][Ee][\"']?([[:space:]]*([,}]|\$))/\\1/g" "$f" |
    grep -nEi "rust-cache|setup-rust-toolchain|actions/cache|sccache|(^[[:space:]]*(-[[:space:]]*)?|[{,][[:space:]]*)${cache_key}[^[:space:]]" || true)"
  if [ -n "$hits" ]; then
    echo "ERROR: $f restores a cache:"
    echo "$hits"
    fail=1
  fi
done

# Every linux-deps use in release.yml opts out of the .deb cache. The input must be in the step's
# own `with:` block, which is the lines between this `uses:` and the next step.
bad="$(awk '
  function close_step() { if (open && !optout) printf "%d\n", at; open = 0; optout = 0 }
  /^[[:space:]]*-[[:space:]]/ { close_step() }
  /uses:[[:space:]]*\.\/\.github\/actions\/linux-deps/ { open = 1; optout = 0; at = NR; next }
  open && /^[[:space:]]*cache:[[:space:]]*"?false"?[[:space:]]*$/ { optout = 1 }
  END { close_step() }
' "$release")"
if [ -n "$bad" ]; then
  echo "ERROR: $release uses linux-deps without cache: \"false\" at line(s): $bad"
  fail=1
fi
if ! grep -q 'uses:[[:space:]]*\./\.github/actions/linux-deps' "$release"; then
  # Not an error by itself, but this script would then be checking nothing it was written for.
  echo "ERROR: $release no longer uses linux-deps; update this check"
  fail=1
fi

# ...and the opt-out has to reach every cache step in the action. Judged per step, whichever way
# round its keys are and whatever starts it. A guard is `inputs.cache == 'true'`, alone or first
# before an `&&`: negated, compared further or beside an `||` it guards nothing.
bad="$(awk '
  function close_step() { if (open && !guarded) printf "%d\n", at; open = 0; guarded = 0 }
  /^[[:space:]]*-[[:space:]]/ { close_step() }
  /^[[:space:]]*(-[[:space:]]+)?if:[[:space:]]*"?(\$\{\{)?[[:space:]]*inputs\.cache[[:space:]]*==[[:space:]]*.true.[[:space:]]*(&&.*|\}\}"?[[:space:]]*|"?[[:space:]]*)$/ && !/\|\|/ { guarded = 1 }
  /uses.?:[[:space:]]*.?actions\/cache/ { seen = 1; open = 1; at = NR }
  END { close_step(); if (!seen) print "no actions/cache step found" }
' "$deps")"
if [ -n "$bad" ]; then
  echo "ERROR: $deps has a cache step not guarded by inputs.cache: $bad"
  fail=1
fi

[ "$fail" -eq 0 ] && echo "release workflows restore no cache"
exit "$fail"
