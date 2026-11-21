#Requires -Version 7.0
<#
.SYNOPSIS
  Two or three RELEASE-mode SyncWatch instances on this PC (portable copies, each with its own
  data folder) using the real Metered setup from .env.local: the closest local equivalent of
  separate users. Never touches %LOCALAPPDATA%\SyncWatch.

.DESCRIPTION
  -Build      builds the exe with the UI embedded (tauri build --no-bundle, release profile).
              With -TestBuild it is a TEST build (SYNCWATCH_TEST_BUILD + VITE_SYNCWATCH_TEST_BUILD):
              still release-optimized, real Metered fetch, manual invites, but it also accepts
              the dev automation variables (auto room, invite file exchange, scenarios), so
              scripts/dev/two-windows.ps1 -ReleasePair can drive it with no mouse or keyboard.
              Without -TestBuild it is exactly a release exe (no automation).
  -Setup      copies the exe (target\release) and the staged runtime files (target\release-stage,
              from scripts/build-release.ps1) into %TEMP%\sw-release-pair\{a,b[,c]}\SyncWatch, or
              unpacks -Zip; writes portable.flag; seeds each data folder with the Metered app name
              + API key (DPAPI, the app's own store; values never shown) and a connection.json
              marked configured and verified (as after a completed setup; the app re-checks at
              start). -RelayOnly sets "Always use relay" in all of them. -Count 3 adds window c.
  -Start      starts them (plain start, for hands-on tests).
  -Stop       stops them and deletes the folders (-Keep keeps them, for the logs).
  Metered use per run: each start re-checks the connection (a few short TURN allocations) and
  each room creation or join fetches fresh credentials (one API call each).

.EXAMPLE
  pwsh -NoProfile -File scripts/dev/release-pair.ps1 -Build -TestBuild -Setup -RelayOnly
.EXAMPLE
  pwsh -NoProfile -File scripts/dev/two-windows.ps1 -Scenario sync -ReleasePair $env:TEMP\sw-release-pair
#>
param(
    [switch]$Build,
    [switch]$TestBuild,
    [switch]$Setup,
    [switch]$Start,
    [switch]$Stop,
    [switch]$Keep,
    [switch]$RelayOnly,
    [ValidateSet(2, 3)][int]$Count = 2,
    [string]$Zip = ''
)
$ErrorActionPreference = 'Stop'
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
Set-Location $root
foreach ($kv in @{ RUSTUP_HOME = 'D:\Rust\rustup'; CARGO_HOME = 'D:\Rust\cargo'; LIBMPV_DIR = 'D:\libs\mpv' }.GetEnumerator()) {
    if (-not [Environment]::GetEnvironmentVariable($kv.Key)) { Set-Item "Env:$($kv.Key)" $kv.Value }
}
if (Test-Path 'D:\Rust\cargo\bin') { $env:PATH = "D:\Rust\cargo\bin;$env:PATH" }
$pnpm = if (Get-Command pnpm.cmd -ErrorAction SilentlyContinue) { 'pnpm.cmd' } else { 'pnpm' }
$base = Join-Path ([IO.Path]::GetTempPath()) 'sw-release-pair'
$who = @('a', 'b', 'c')[0..($Count - 1)]
function Exe($w) { Join-Path $base "$w\SyncWatch\syncwatch.exe" }

if ($Stop) {
    Get-Process syncwatch -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$base*" } | Stop-Process -Force
    Start-Sleep -Seconds 2
    if (-not $Keep) { Remove-Item -Recurse -Force $base -ErrorAction SilentlyContinue; Write-Host 'Stopped; folders removed.' } else { Write-Host "Stopped; folders kept in $base" }
}

if ($Build) {
    if (-not (Test-Path target\release-stage\libmpv-2.dll)) { throw 'No staged runtime files: run scripts/build-release.ps1 once (it fills target\release-stage).' }
    if ($TestBuild) { $env:SYNCWATCH_TEST_BUILD = '1'; $env:VITE_SYNCWATCH_TEST_BUILD = '1' } else { Remove-Item Env:SYNCWATCH_TEST_BUILD, Env:VITE_SYNCWATCH_TEST_BUILD -ErrorAction SilentlyContinue }
    try {
        & $pnpm --dir apps/desktop tauri build --no-bundle 2>&1 | Select-Object -Last 3
        if ($LASTEXITCODE) { throw 'the build failed' }
    } finally {
        Remove-Item Env:SYNCWATCH_TEST_BUILD, Env:VITE_SYNCWATCH_TEST_BUILD -ErrorAction SilentlyContinue
    }
    Write-Host "Built target\release\syncwatch.exe ($(if ($TestBuild) { 'TEST build: automation enabled' } else { 'release build' }))"
}

if ($Setup) {
    if (Get-Process syncwatch -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$base*" }) { throw 'The instances are running: -Stop first.' }
    Remove-Item -Recurse -Force $base -ErrorAction SilentlyContinue
    foreach ($w in $who) {
        $dir = Join-Path $base $w
        $app = Join-Path $dir 'SyncWatch'
        if ($Zip) {
            Expand-Archive $Zip -DestinationPath $dir
        } else {
            New-Item -ItemType Directory -Force $app | Out-Null
            Copy-Item target\release\syncwatch.exe $app
            Get-ChildItem target\release-stage -Exclude 'portable', 'tauri.release.json' | Copy-Item -Destination $app -Recurse
            New-Item -ItemType File (Join-Path $app 'portable.flag') -Force | Out-Null
        }
        if (-not (Test-Path (Join-Path $app 'portable.flag'))) { throw 'no portable.flag' }
        $data = Join-Path $app 'data'
        $out = cargo run -q -p syncwatch-ice-config --example seed_portable -- $data 2>&1
        if ("$out".Trim() -ne 'seeded') { throw "seeding $w failed: $out" }
        $settings = [ordered]@{ configured = $true; mode = 'apiKey'; hostnames = @(); verified = $true; verifiedAtUnix = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds(); relayOnly = [bool]$RelayOnly; lastResults = @() }
        $settings | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $data 'config\connection.json')
        Write-Host "Instance $w ready: $(Exe $w) (relay only: $([bool]$RelayOnly))"
    }
}

if ($Start) {
    foreach ($w in $who) {
        $p = Start-Process (Exe $w) -PassThru
        Write-Host "Instance $w started (pid $($p.Id))"
        Start-Sleep -Seconds 2
    }
}
