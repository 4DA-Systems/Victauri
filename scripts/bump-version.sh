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

OLD_VERSION=$(grep -oP 'version\s*=\s*"\K[0-9]+\.[0-9]+\.[0-9]+' "$ROOT/Cargo.toml" | head -1)

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
            sed -i "s|$(echo "$old" | sed 's/[&/\]/\\&/g')|$(echo "$new" | sed 's/[&/\]/\\&/g')|g" "$path"
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
    sed -i -E "s|(victauri-(core|macros|plugin|test) = \{ version = \")[^\"]+(\")|\1$NEW_VERSION\3|g" "$ROOT/Cargo.toml"
    echo "  OK    Cargo.toml [workspace.dependencies] victauri-* pins"
fi

# NOTE: the browser extensions (chrome/firefox/npm) and the VS Code extension are
# DECOUPLED from the core workspace version — they ship on their own cadence and are
# versioned independently (bump them in their own package.json/manifest when you
# actually change them). Force-bumping them on every core release made them rot
# (e.g. a release tag with no extension changes). When an extension genuinely
# changes, bump the top-level "version" field in its own package.json / manifest.json.

# 8. Composite action default CLI version
replace_in_file ".github/actions/victauri-test/action.yml" "default: \"$OLD_VERSION\"" "default: \"$NEW_VERSION\"" "victauri-test action CLI pin"

# 9. JS bridge version — NO LONGER bumped here. init_script() injects the crate version
#    (env!("CARGO_PKG_VERSION")) into the __VICTAURI_BRIDGE_VERSION__ placeholder, so the JS
#    bridge version can never drift (VIC-2); the bridge tests assert against CARGO_PKG_VERSION.

# 11-12. Docs
replace_in_file "docs/src/getting-started.md" "\"version\":\"$OLD_VERSION\"" "\"version\":\"$NEW_VERSION\"" "docs getting-started"
replace_in_file "docs/src/compatibility.md" "\"bridge_version\": \"$OLD_VERSION\"" "\"bridge_version\": \"$NEW_VERSION\"" "docs compatibility"

# 13. CLAUDE.md — replace all vX.Y.Z references
if [[ -f "$ROOT/CLAUDE.md" ]]; then
    if grep -qF "v$OLD_VERSION" "$ROOT/CLAUDE.md"; then
        if $DRY_RUN; then
            echo "  WOULD CLAUDE.md version references"
        else
            sed -i "s/v$OLD_VERSION/v$NEW_VERSION/g" "$ROOT/CLAUDE.md"
            echo "  OK    CLAUDE.md version references"
        fi
    else
        echo "  SKIP  CLAUDE.md (no v$OLD_VERSION references)"
    fi
fi

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
