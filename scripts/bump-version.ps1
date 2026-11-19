#Requires -Version 7.0
<#
.SYNOPSIS
  Sets the SyncWatch version everywhere it is written (the MSI version follows tauri.conf.json):
  Cargo.toml (workspace), apps/desktop/package.json, apps/desktop/src-tauri/tauri.conf.json.
  Updates Cargo.lock. Does not commit.

.EXAMPLE
  pwsh -NoProfile -File scripts/bump-version.ps1 -Version 0.1.1
#>
param([Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = 'Stop'
if ($Version -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$') {
    Write-Host 'Use a version like 0.1.1 (major.minor.patch, no prefix).' -ForegroundColor Red; exit 1
}
$parts = $Version.Split('.') | ForEach-Object { [int]$_ }
if ($parts[0] -gt 255 -or $parts[1] -gt 255 -or $parts[2] -gt 65535) {
    Write-Host 'MSI versions allow at most 255.255.65535.' -ForegroundColor Red; exit 1
}
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo
function Replace-Once([string]$path, [string]$pattern, [string]$replacement) {
    $t = Get-Content $path -Raw
    $n = ([regex]::Matches($t, $pattern)).Count
    if ($n -ne 1) { throw "$path : expected one version field, found $n" }
    Set-Content -NoNewline -Encoding utf8 $path ([regex]::Replace($t, $pattern, $replacement))
}
Replace-Once 'Cargo.toml' '(?m)(?<=^\[workspace\.package\][\s\S]*?^version = ")[^"]+(?=")' $Version
Replace-Once 'apps\desktop\package.json' '(?<="version": ")[^"]+(?=")' $Version
Replace-Once 'apps\desktop\src-tauri\tauri.conf.json' '(?<="version": ")[^"]+(?=")' $Version
if (Test-Path 'D:\Rust\cargo\bin') { $env:PATH = "D:\Rust\cargo\bin;$env:PATH" }
cargo update --workspace --offline *> $null
Write-Host "Version set to $Version in Cargo.toml, apps/desktop/package.json and tauri.conf.json."
Write-Host "Next: add a '## $Version' section to CHANGELOG.md and commit both."
