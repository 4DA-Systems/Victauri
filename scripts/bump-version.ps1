#!/usr/bin/env pwsh
# bump-version.ps1 — Bumps all version references across the Victauri workspace.
#
# Usage:
#   .\scripts\bump-version.ps1 0.5.4
#   .\scripts\bump-version.ps1 0.6.0 -DryRun
#
# What it updates:
#   - Cargo.toml [workspace.package] version
#   - Cargo.toml [workspace.dependencies] victauri-* pins
#   - Cargo.lock (via cargo check)
#   - .github/actions/victauri-test/action.yml (pinned CLI install default)
#   - crates/victauri-plugin/src/js_bridge.rs (bridge version constant)
#   - crates/victauri-plugin/tests/bridge_tests.rs (version assertions)
#   - docs/src/getting-started.md (example output)
#   - docs/src/compatibility.md (example output)
#   - CLAUDE.md (Current State header and version ref)
#
# Does NOT update:
#   - CHANGELOG.md (requires human-written release notes)
#   - MIGRATION.md (requires human-written migration guide)
#   - README.md (intentionally version-free)
#   - Test counts (require running tests to get actual numbers)

param(
    [Parameter(Mandatory=$true, Position=0)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$NewVersion,

    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not (Test-Path "$root\Cargo.toml")) {
    $root = Split-Path -Parent $PSScriptRoot
}
if (-not (Test-Path "$root\Cargo.toml")) {
    Write-Error "Cannot find Cargo.toml — run from the victauri repo root or scripts/ dir"
    exit 1
}

# Detect current version from Cargo.toml
$cargoToml = Get-Content "$root\Cargo.toml" -Raw
if ($cargoToml -match 'version\s*=\s*"(\d+\.\d+\.\d+)"') {
    $OldVersion = $matches[1]
} else {
    Write-Error "Cannot detect current version from Cargo.toml"
    exit 1
}

if ($OldVersion -eq $NewVersion) {
    Write-Host "Already at version $NewVersion — nothing to do." -ForegroundColor Yellow
    exit 0
}

Write-Host "Bumping $OldVersion -> $NewVersion" -ForegroundColor Cyan

# Helper: replace version in a file
function Update-File {
    param(
        [string]$Path,
        [string]$Pattern,
        [string]$Replacement,
        [string]$Description
    )
    $fullPath = Join-Path $root $Path
    if (-not (Test-Path $fullPath)) {
        Write-Host "  SKIP $Path (not found)" -ForegroundColor Yellow
        return
    }
    $content = Get-Content $fullPath -Raw
    if ($content -match [regex]::Escape($Pattern)) {
        if ($DryRun) {
            Write-Host "  WOULD $Description" -ForegroundColor DarkGray
        } else {
            $content = $content -replace [regex]::Escape($Pattern), $Replacement
            Set-Content $fullPath $content -NoNewline
            Write-Host "  OK    $Description" -ForegroundColor Green
        }
    } else {
        Write-Host "  SKIP  $Description (pattern not found)" -ForegroundColor Yellow
    }
}

# 1. Cargo.toml workspace version
Update-File "Cargo.toml" "version = `"$OldVersion`"" "version = `"$NewVersion`"" "Cargo.toml workspace version"

# 1b. Cargo.toml [workspace.dependencies] inter-crate pins.
# Update-File matches an exact old version, but these pins can lag behind the
# workspace version (they did on 0.6.0 and 0.7.0, breaking `cargo update`). Set
# them structurally to the new version regardless of their current value.
$cargoToml = Join-Path $root "Cargo.toml"
$pinPattern = '(victauri-(?:core|macros|plugin|test)\s*=\s*\{\s*version\s*=\s*")[^"]+(")'
$cargoContent = Get-Content $cargoToml -Raw
if ($cargoContent -match $pinPattern) {
    if ($DryRun) {
        Write-Host "  WOULD Cargo.toml [workspace.dependencies] victauri-* pins" -ForegroundColor DarkGray
    } else {
        $cargoContent = [regex]::Replace($cargoContent, $pinPattern, "`${1}$NewVersion`${2}")
        Set-Content $cargoToml $cargoContent -NoNewline
        Write-Host "  OK    Cargo.toml [workspace.dependencies] victauri-* pins" -ForegroundColor Green
    }
}

# NOTE: the VS Code extension is DECOUPLED from the core workspace version. It ships
# on its own cadence (`vscode-v*` tag) and is versioned independently — bump the
# top-level "version" field in editors/vscode/package.json only when it actually changes.

# 8. Composite action default CLI version
Update-File ".github\actions\victauri-test\action.yml" "default: `"$OldVersion`"" "default: `"$NewVersion`"" "victauri-test action CLI pin"

# 9. JS bridge version — NO LONGER bumped here. `init_script()` injects the crate version
#    (env!("CARGO_PKG_VERSION")) into the `__VICTAURI_BRIDGE_VERSION__` placeholder, so the JS
#    bridge version can never drift from the crate version (VIC-2). The bridge tests assert
#    against CARGO_PKG_VERSION, so they need no per-release edit either.

# 11. docs/src/getting-started.md version in example output
Update-File "docs\src\getting-started.md" "`"version`":`"$OldVersion`"" "`"version`":`"$NewVersion`"" "docs getting-started.md example"

# 12. docs/src/compatibility.md bridge_version
Update-File "docs\src\compatibility.md" "`"bridge_version`": `"$OldVersion`"" "`"bridge_version`": `"$NewVersion`"" "docs compatibility.md bridge_version"

# 13. CLAUDE.md Current State version ref
$claudeMd = Join-Path $root "CLAUDE.md"
if (Test-Path $claudeMd) {
    $content = Get-Content $claudeMd -Raw
    $updated = $content -replace [regex]::Escape("v$OldVersion"), "v$NewVersion"
    if ($updated -ne $content) {
        if ($DryRun) {
            Write-Host "  WOULD CLAUDE.md version references" -ForegroundColor DarkGray
        } else {
            Set-Content $claudeMd $updated -NoNewline
            Write-Host "  OK    CLAUDE.md version references" -ForegroundColor Green
        }
    } else {
        Write-Host "  SKIP  CLAUDE.md (no v$OldVersion references)" -ForegroundColor Yellow
    }
}

# 14. Update Cargo.lock via cargo check
if (-not $DryRun) {
    Write-Host "`nUpdating Cargo.lock..." -ForegroundColor Cyan
    Push-Location $root
    try {
        cargo check --workspace 2>&1 | Select-Object -Last 1
    } finally {
        Pop-Location
    }
}

Write-Host "`n--- Version bump complete: $OldVersion -> $NewVersion ---" -ForegroundColor Cyan

if ($DryRun) {
    Write-Host "(dry run — no files were modified)" -ForegroundColor DarkGray
}

Write-Host @"

Remaining manual steps:
  1. Update CHANGELOG.md with release notes
  2. Update MIGRATION.md if there are breaking/behavior changes
  3. Update CLAUDE.md Current State date and new feature descriptions
  4. Run: cargo test --workspace
  5. Run: cargo clippy --workspace --all-targets
  6. Commit, push, publish: cargo publish -p <crate> for each crate
"@ -ForegroundColor DarkYellow
