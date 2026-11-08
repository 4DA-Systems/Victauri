#!/usr/bin/env bash
# semver-check-plugin.sh — semver-check victauri-plugin against the last release tag.
#
# `cargo semver-checks check-release --workspace` cannot check victauri-plugin. It builds each crate
# in a placeholder project that ignores Cargo.lock, where the plugin's
# `tauri = ">=2.0.0, <=3.0.0-alpha.N"` range resolves the newest Tauri 3 pre-release (and its
# feature heuristic turns on `tauri-v3`), so the build fails before anything is checked. CI's
# informational step only warned about that, so from Tauri 3.0.0-alpha.5 (2026-10-14) on, no run
# checked the plugin at all.
#
# This builds the plugin's rustdoc JSON on both sides from their OWN lockfiles (Tauri 2, default
# features: everything but `tauri-v3`) and compares the two. The baseline is the newest vX.Y.Z tag
# merged into HEAD, exported with `git archive` (no worktree: adding and removing one can flip this
# repo's core.bare).
#
# Usage: ./scripts/semver-check-plugin.sh [cargo-semver-checks args, e.g. --release-type patch]
#   SEMVER_BASELINE_TAG=v0.10.3 overrides the baseline tag.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BASE_TAG="${SEMVER_BASELINE_TAG:-$(git tag --list 'v[0-9]*.[0-9]*.[0-9]*' --merged HEAD --sort=-v:refname | head -1)}"
if [[ -z "$BASE_TAG" ]]; then
    echo "error: no vX.Y.Z release tag is reachable from HEAD (a shallow clone? run: git fetch --tags)" >&2
    exit 2
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/base"
git archive --format=tar "$BASE_TAG" | tar -x -C "$WORK/base"

# One target directory for both sides: the dependencies (Tauri and the rest) are shared, only the
# workspace crates build twice. The JSON is copied out after each build because both write the
# same path.
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
DOC_JSON="$TARGET/doc/victauri_plugin.json"

build_json() {
    local src="$1" out="$2"
    rm -f "$DOC_JSON"
    (cd "$src" && RUSTC_BOOTSTRAP=1 CARGO_TARGET_DIR="$TARGET" \
        cargo rustdoc -p victauri-plugin --lib --locked -- -Z unstable-options --output-format json)
    if [[ ! -f "$DOC_JSON" ]]; then
        echo "error: cargo rustdoc wrote no $DOC_JSON for $src" >&2
        exit 1
    fi
    cp "$DOC_JSON" "$out"
}

echo "victauri-plugin: $(git describe --always HEAD) (current) vs $BASE_TAG (baseline), both from their Cargo.lock"
build_json "$ROOT" "$WORK/current.json"
build_json "$WORK/base" "$WORK/baseline.json"

cargo semver-checks check-release \
    --current-rustdoc "$WORK/current.json" \
    --baseline-rustdoc "$WORK/baseline.json" \
    "$@"
