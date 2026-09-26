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
#   - server.json (MCP Registry manifest versions)
#   - docs/src/getting-started.md (example output)
#   - docs/src/compatibility.md (example output)
#   - `victauri-<crate> = "X.Y"` install pins in README.md, docs/src/{getting-started,testing,
#     testing-tauri-apps}.md and crates/victauri-{plugin,test,macros}/README.md (set to the
#     new major.minor)
#   - `victauri-test@vX.Y.Z` GitHub Action refs in README.md and docs/src/testing*.md
#
# Does NOT update:
#   - CHANGELOG.md (requires human-written release notes)
#   - MIGRATION.md (requires human-written migration guide)
#   - CLAUDE.md (requires human-written Current State entry)
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

# 7. server.json — the MCP Registry manifest. Covers BOTH the manifest `version` and the
#    cargo package `version` (Update-File replaces every occurrence). This was missed by the
#    0.8.7 release and had to be caught by hand, so it is automated here.
Update-File "server.json" "`"version`": `"$OldVersion`"" "`"version`": `"$NewVersion`"" "server.json versions"

# 8. Composite action default CLI version
Update-File ".github\actions\victauri-test\action.yml" "default: `"$OldVersion`"" "default: `"$NewVersion`"" "victauri-test action CLI pin"

# 9. JS bridge version — NO LONGER bumped here. `init_script()` injects the crate version
#    (env!("CARGO_PKG_VERSION")) into the `__VICTAURI_BRIDGE_VERSION__` placeholder, so the JS
#    bridge version can never drift from the crate version (VIC-2). The bridge tests assert
#    against CARGO_PKG_VERSION, so they need no per-release edit either.

# 10. Version-pinned doc lines, set structurally (like 1b) so they can never lag:
#   - `victauri-<crate> = "X.Y"` install pins (and `{ version = "X.Y", ... }`) -> new major.minor
#   - `victauri-test@vX.Y.Z` GitHub Action refs -> new full version
$newMajorMinor = ($NewVersion -split '\.')[0..1] -join '.'
$pinDocs = @('README.md', 'docs\src\getting-started.md', 'docs\src\testing.md',
    'docs\src\testing-tauri-apps.md', 'crates\victauri-plugin\README.md',
    'crates\victauri-test\README.md', 'crates\victauri-macros\README.md')
$actionDocs = @('README.md', 'docs\src\testing.md', 'docs\src\testing-tauri-apps.md')
$installPin = '(victauri-(?:core|macros|plugin|test)\s*=\s*(?:\{\s*version\s*=\s*)?")\d+\.\d+(")'
$actionRef = '(victauri-test@v)\d+\.\d+\.\d+'
foreach ($doc in ($pinDocs + $actionDocs | Select-Object -Unique)) {
    $docPath = Join-Path $root $doc
    if (-not (Test-Path $docPath)) {
        Write-Host "  SKIP $doc (not found)" -ForegroundColor Yellow
        continue
    }
    $docContent = Get-Content $docPath -Raw
    $updated = $docContent
    if ($pinDocs -contains $doc) {
        $updated = [regex]::Replace($updated, $installPin, "`${1}$newMajorMinor`${2}")
    }
    if ($actionDocs -contains $doc) {
        $updated = [regex]::Replace($updated, $actionRef, "`${1}$NewVersion")
    }
    if ($updated -ne $docContent) {
        if ($DryRun) {
            Write-Host "  WOULD $doc version pins / action refs" -ForegroundColor DarkGray
        } else {
            Set-Content $docPath $updated -NoNewline
            Write-Host "  OK    $doc version pins / action refs" -ForegroundColor Green
        }
    } else {
        Write-Host "  SKIP  $doc version pins / action refs (already current)" -ForegroundColor Yellow
    }
}

# 11. docs/src/getting-started.md version in example output
Update-File "docs\src\getting-started.md" "`"version`":`"$OldVersion`"" "`"version`":`"$NewVersion`"" "docs getting-started.md example"

# 12. docs/src/compatibility.md bridge_version
Update-File "docs\src\compatibility.md" "`"bridge_version`": `"$OldVersion`"" "`"bridge_version`": `"$NewVersion`"" "docs compatibility.md bridge_version"

# 13. Update Cargo.lock via cargo check
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
    Write-Host "(dry run - no files were modified)" -ForegroundColor DarkGray
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
