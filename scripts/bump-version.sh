#!/usr/bin/env bash
# bump-version.sh — Bumps all version references across the Victauri workspace.
#
# Usage:
#   ./scripts/bump-version.sh 0.5.4
#   ./scripts/bump-version.sh 0.6.0 --dry-run
#
# See bump-version.ps1 header for the full list of files updated.

set -euo pipefail

NEW_VERSION="${1:-}"
DRY_RUN=false
if [[ "${2:-}" == "--dry-run" ]]; then
    DRY_RUN=true
fi

if [[ -z "$NEW_VERSION" ]] || ! [[ "$NEW_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Usage: $0 <new-version> [--dry-run]"
    echo "Example: $0 0.5.4"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if [[ ! -f "$ROOT/Cargo.toml" ]]; then
    echo "Error: Cannot find Cargo.toml at $ROOT"
    exit 1
fi

# Portable (GNU + BSD/macOS): no `grep -P`, no bare `sed -i`.
# In-place sed that works on both GNU sed and BSD sed: `-i.bak` is accepted by both
# (BSD requires a suffix argument; GNU accepts it attached), then the backup is removed.
# The target file must be the LAST argument.
sed_inplace() {
    local target="${!#}"
    sed -i.bak "$@" && rm -f "$target.bak"
}

OLD_VERSION=$(sed -n -E 's/^[[:space:]]*version[[:space:]]*=[[:space:]]*"([0-9]+\.[0-9]+\.[0-9]+)".*/\1/p' "$ROOT/Cargo.toml" | head -1)

if [[ -z "$OLD_VERSION" ]]; then
    echo "Error: Cannot detect current version from Cargo.toml"
    exit 1
fi

if [[ "$OLD_VERSION" == "$NEW_VERSION" ]]; then
    echo "Already at version $NEW_VERSION — nothing to do."
    exit 0
fi

echo "Bumping $OLD_VERSION -> $NEW_VERSION"

replace_in_file() {
    local file="$1" old="$2" new="$3" desc="$4"
    local path="$ROOT/$file"
    if [[ ! -f "$path" ]]; then
        echo "  SKIP $desc (not found)"
        return
    fi
    if grep -qF "$old" "$path"; then
        if $DRY_RUN; then
            echo "  WOULD $desc"
        else
            sed_inplace "s|$(echo "$old" | sed 's/[&/\]/\\&/g')|$(echo "$new" | sed 's/[&/\]/\\&/g')|g" "$path"
            echo "  OK    $desc"
        fi
    else
        echo "  SKIP  $desc (pattern not found)"
    fi
}

# 1. Cargo.toml
replace_in_file "Cargo.toml" "version = \"$OLD_VERSION\"" "version = \"$NEW_VERSION\"" "Cargo.toml workspace version"

# 1b. Cargo.toml [workspace.dependencies] inter-crate pins. These can lag behind
# the workspace version (they did on 0.6.0 and 0.7.0, breaking `cargo update`),
# so set them structurally to the new version regardless of their current value.
if [[ "$DRY_RUN" == true ]]; then
    echo "  WOULD update Cargo.toml [workspace.dependencies] victauri-* pins"
else
    # `#` delimiter: the pattern's `(core|macros|...)` alternation contains `|`.
    sed_inplace -E "s#(victauri-(core|macros|plugin|test) = \{ version = \")[^\"]+(\")#\1$NEW_VERSION\3#g" "$ROOT/Cargo.toml"
    echo "  OK    Cargo.toml [workspace.dependencies] victauri-* pins"
fi

# NOTE: the VS Code extension is DECOUPLED from the core workspace version — it ships
# on its own cadence (vscode-v* tag) and is versioned independently. Bump the
# top-level "version" field in editors/vscode/package.json only when it changes.

# 8. Composite action default CLI version
# server.json — the MCP Registry manifest. Covers BOTH the manifest `version` and the cargo
# package `version` (replace_in_file substitutes every occurrence). Missed by the 0.8.7 release
# and caught by hand, so it is automated here.
replace_in_file "server.json" "\"version\": \"$OLD_VERSION\"" "\"version\": \"$NEW_VERSION\"" "server.json versions"

replace_in_file ".github/actions/victauri-test/action.yml" "default: \"$OLD_VERSION\"" "default: \"$NEW_VERSION\"" "victauri-test action CLI pin"

# 9. JS bridge version — NO LONGER bumped here. init_script() injects the crate version
#    (env!("CARGO_PKG_VERSION")) into the __VICTAURI_BRIDGE_VERSION__ placeholder, so the JS
#    bridge version can never drift (VIC-2); the bridge tests assert against CARGO_PKG_VERSION.

# 10. Version-pinned doc lines, set structurally (like 1b) so they can never lag:
#   - `victauri-<crate> = "X.Y"` install pins (and `{ version = "X.Y", ... }`) -> new major.minor
#   - `victauri-test@vX.Y.Z` GitHub Action refs -> new full version
NEW_MM="${NEW_VERSION%.*}"
PIN_DOCS=(README.md docs/src/getting-started.md docs/src/testing.md docs/src/testing-tauri-apps.md
          crates/victauri-plugin/README.md crates/victauri-test/README.md crates/victauri-macros/README.md)
ACTION_DOCS=(README.md docs/src/testing.md docs/src/testing-tauri-apps.md)
for f in "${PIN_DOCS[@]}"; do
    [[ -f "$ROOT/$f" ]] || { echo "  SKIP  $f install pins (not found)"; continue; }
    if $DRY_RUN; then
        echo "  WOULD $f install pins -> \"$NEW_MM\""
    else
        sed_inplace -E "s#(victauri-(core|macros|plugin|test) *= *(\{ *version *= *)?\")[0-9]+\.[0-9]+(\")#\1$NEW_MM\4#g" "$ROOT/$f"
        echo "  OK    $f install pins -> \"$NEW_MM\""
    fi
done
for f in "${ACTION_DOCS[@]}"; do
    [[ -f "$ROOT/$f" ]] || { echo "  SKIP  $f action refs (not found)"; continue; }
    if $DRY_RUN; then
        echo "  WOULD $f victauri-test@v$NEW_VERSION"
    else
        sed_inplace -E "s|(victauri-test@v)[0-9]+\.[0-9]+\.[0-9]+|\1$NEW_VERSION|g" "$ROOT/$f"
        echo "  OK    $f victauri-test@v$NEW_VERSION"
    fi
done

# 11-12. Docs
replace_in_file "docs/src/getting-started.md" "\"version\":\"$OLD_VERSION\"" "\"version\":\"$NEW_VERSION\"" "docs getting-started"
replace_in_file "docs/src/compatibility.md" "\"bridge_version\": \"$OLD_VERSION\"" "\"bridge_version\": \"$NEW_VERSION\"" "docs compatibility"

# 13. Cargo.lock
if ! $DRY_RUN; then
    echo ""
    echo "Updating Cargo.lock..."
    (cd "$ROOT" && cargo check --workspace 2>&1 | tail -1)
fi

echo ""
echo "--- Version bump complete: $OLD_VERSION -> $NEW_VERSION ---"

if $DRY_RUN; then
    echo "(dry run — no files were modified)"
fi

cat <<'MANUAL'

Remaining manual steps:
  1. Update CHANGELOG.md with release notes
  2. Update MIGRATION.md if there are breaking/behavior changes
  3. Update CLAUDE.md Current State date and new feature descriptions
  4. Run: cargo test --workspace
  5. Run: cargo clippy --workspace --all-targets
  6. Commit, push, publish: cargo publish -p <crate> for each crate
MANUAL
