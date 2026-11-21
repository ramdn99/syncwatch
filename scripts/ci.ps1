#Requires -Version 7
<#
.SYNOPSIS
  The CI sequence on Windows (spec section 12): build, clippy -D warnings, rustfmt, tests, TLS
  tree, cargo-deny, cargo-audit, import check, secret scan, frontend lint/test/build.
.PARAMETER Quick
  Skip the slow torrent integration tests (they run in full CI).
#>
param([switch]$Quick)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
function Step($name, [scriptblock]$b) {
    Write-Host "=== $name"
    & $b
    if ($LASTEXITCODE -ne 0) { Write-Host "FAIL  $name" -ForegroundColor Red; exit 1 }
}
Step 'build' { cargo build --workspace --locked }
Step 'clippy' { cargo clippy --workspace --all-targets --locked -- -D warnings }
Step 'rustfmt' { cargo fmt --all -- --check }
if ($Quick) {
    Step 'tests' { cargo test --workspace --locked --exclude syncwatch-torrent }
    Step 'torrent unit tests' { cargo test -p syncwatch-torrent --lib --locked }
} else {
    Step 'tests' { cargo test --workspace --locked }
}
Step 'tls tree' { pwsh -NoProfile -File scripts/check-tls-tree.ps1 }
Step 'cargo-deny' { cargo deny --locked check bans advisories sources }
Step 'cargo-audit' { cargo audit }
Step 'imports' { pwsh -NoProfile -File scripts/verify-imports.ps1 }
Step 'secret scan' { pwsh -NoProfile -File scripts/secret-scan.ps1 -Mode All }
Step 'frontend lint' { pnpm.cmd --dir apps/desktop lint }
Step 'frontend test' { pnpm.cmd --dir apps/desktop test }
# Real-DOM tests in the installed Microsoft Edge (real keys, clicks and form submission).
Step 'frontend browser test' { pnpm.cmd --dir apps/desktop test:browser }
Step 'frontend build' { pnpm.cmd --dir apps/desktop build }
Step 'emoji scan' { node apps/desktop/scripts/emoji-scan.mjs }
Write-Host 'PASS  ci'
