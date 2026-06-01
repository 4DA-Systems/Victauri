#!/usr/bin/env pwsh
# Local pre-publish gate. Runs everything that CAN be checked on a dev machine so a
# fmt/clippy/test failure is caught BEFORE pushing (instead of after a ~16-minute CI round
# trip). Run it before you push a release commit.
#
# It does NOT replace CI, and is deliberately not the publish gate:
#   * cross-platform bugs (macOS/Linux) can only be caught by CI's matrix — e.g. 0.7.4
#     shipped a macOS-only discovery bug that no Windows run could see;
#   * the real-app E2E (a live demo-app) is a separate CI job;
# so the release workflow refuses to publish unless the FULL CI is green for the commit
# (the `require-ci-green` job). Preflight just makes the fast failures fast.

$failed = @()
function Run([string]$name, [scriptblock]$cmd) {
    Write-Host "`n=== $name ===" -ForegroundColor Cyan
    & $cmd
    if ($LASTEXITCODE -ne 0) { $script:failed += $name; Write-Host "FAILED: $name" -ForegroundColor Red }
    else { Write-Host "ok: $name" -ForegroundColor Green }
}

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

Run "cargo fmt --check"   { cargo fmt --all -- --check }
Run "clippy"              { cargo clippy --workspace --all-targets --features sqlite -- -D warnings }
Run "workspace tests"     { cargo test --workspace --features sqlite }
Run "chrome bridge tests" { Push-Location extensions/chrome/tests; npx vitest run; $code = $LASTEXITCODE; Pop-Location; $global:LASTEXITCODE = $code }

Write-Host ""
if ($failed.Count -gt 0) {
    Write-Host "PREFLIGHT FAILED: $($failed -join ', ')" -ForegroundColor Red
    Write-Host "Fix these before pushing. (macOS/Linux + the real-app E2E are still verified by CI.)" -ForegroundColor Yellow
    exit 1
}
Write-Host "PREFLIGHT PASSED (local gate)." -ForegroundColor Green
Write-Host "Next: push, wait for CI to go GREEN on all platforms, then bump + tag. The release" -ForegroundColor Green
Write-Host "workflow will refuse to publish unless CI is green for that commit." -ForegroundColor Green
