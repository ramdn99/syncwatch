#Requires -Version 7.0
<#
.SYNOPSIS
  Prepares the Windows Sandbox test (docs/clean-machine-test.md) on the host. Does not start
  the sandbox and installs nothing on this PC.

.DESCRIPTION
  Fills dist\sandbox (mapped READ-ONLY into the sandbox as C:\SyncWatchTest):
    SyncWatch-0.0.1-x64.msi        test build (SYNCWATCH_TEST_BUILD), signed with a throwaway key
    update\SyncWatch-0.0.2-x64.msi test build 0.0.2, the "update"
    update\latest.json(.sig)       signed manifest pointing to http://127.0.0.1:8765/
    test-updater.key.pub           the throwaway PUBLIC key (the private key is deleted)
    serve-update.ps1, auto-test.ps1
    room-test.ps1, media\         the two-instance room test (auto-test.ps1 step 4b) and its
                                   generated media (scripts/dev/make-test-media.ps1; never
                                   D:\media\test.mkv)
    release\SyncWatch-<v>-x64.msi  (with -IncludeRelease) the newest MSI from dist\test-release
                                   (testing builds of HEAD) or else dist\release, for the
                                   hands-on checklist. Kept when this script runs again.
  and creates dist\sandbox-results (mapped writable as C:\SyncWatchResults).

  The two test builds take about 5 minutes each. The test MSIs accept a local update server
  and a test key; they are marked TEST-BUILD and never published.

.EXAMPLE
  pwsh -NoProfile -File scripts/sandbox-prepare.ps1 -IncludeRelease
#>
param(
    [switch]$IncludeRelease,
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
$pnpm = if (Get-Command pnpm.cmd -ErrorAction SilentlyContinue) { 'pnpm.cmd' } else { 'pnpm' }
$box = Join-Path $root 'dist\sandbox'
$results = Join-Path $root 'dist\sandbox-results'
New-Item -ItemType Directory -Force $results | Out-Null
# Keep release\ (the hands-on MSI) across re-runs; everything else is rebuilt.
if (Test-Path $box) { Get-ChildItem $box -Force | Where-Object Name -NE 'release' | Remove-Item -Recurse -Force }
New-Item -ItemType Directory -Force (Join-Path $box 'update') | Out-Null

$keyDir = Join-Path ([IO.Path]::GetTempPath()) "sw-sandbox-key-$PID"
New-Item -ItemType Directory -Force $keyDir | Out-Null
$key = Join-Path $keyDir 'sandbox-test.key'
try {
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = [Convert]::ToBase64String([Security.Cryptography.RandomNumberGenerator]::GetBytes(24))
    & $pnpm --dir apps/desktop tauri signer generate -w $key -p $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD --ci -f *> $null
    if (-not (Test-Path "$key.pub")) { throw 'could not make the throwaway test key' }
    Copy-Item "$key.pub" (Join-Path $box 'test-updater.key.pub')

    foreach ($v in '0.0.1', '0.0.2') {
        if (-not $SkipBuild) {
            Write-Host "== test build $v"
            pwsh -NoProfile -File scripts/build-release.ps1 -TestBuild -Version $v -UpdaterKey $key -SkipVerify
            if ($LASTEXITCODE) { throw "test build $v failed (logs in dist\test\$v\logs)" }
        }
    }
    Copy-Item 'dist\test\0.0.1\SyncWatch-0.0.1-x64.msi' $box
    $msi2 = 'dist\test\0.0.2\SyncWatch-0.0.2-x64.msi'
    Copy-Item $msi2, "$msi2.sig" (Join-Path $box 'update')

    $manifest = [ordered]@{
        version   = '0.0.2'
        notes     = 'Sandbox update test (test build).'
        pub_date  = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
        platforms = [ordered]@{
            'windows-x86_64' = [ordered]@{
                signature = (Get-Content "$msi2.sig" -Raw).Trim()
                url       = 'http://127.0.0.1:8765/SyncWatch-0.0.2-x64.msi'
                sha256    = (Get-FileHash $msi2 -Algorithm SHA256).Hash.ToLowerInvariant()
            }
        }
    }
    $latest = Join-Path $box 'update\latest.json'
    [IO.File]::WriteAllText($latest, ($manifest | ConvertTo-Json -Depth 5))
    & $pnpm --dir apps/desktop tauri signer sign -f $key $latest *> $null
    if (-not (Test-Path "$latest.sig")) { throw 'could not sign latest.json' }
} finally {
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $keyDir -ErrorAction SilentlyContinue
}
Copy-Item scripts\sandbox\serve-update.ps1, scripts\sandbox\auto-test.ps1, scripts\sandbox\room-test.ps1 $box
$media = Join-Path ([IO.Path]::GetTempPath()) 'syncwatch-media'
if (-not (Test-Path (Join-Path $media 'long.mkv'))) { pwsh -NoProfile -File scripts/dev/make-test-media.ps1 | Out-Host }
New-Item -ItemType Directory -Force (Join-Path $box 'media') | Out-Null
Copy-Item (Join-Path $media 'long.mkv'), (Join-Path $media 'extra-cp1252.srt'), (Join-Path $media 'extra-audio.mka') (Join-Path $box 'media')
if ($IncludeRelease) {
    $m = @('dist\test-release', 'dist\release') | Where-Object { Test-Path $_ } |
        ForEach-Object { Get-ChildItem $_ -Recurse -Filter 'SyncWatch-*-x64.msi' } |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($m) {
        $rdir = Join-Path $box 'release'
        New-Item -ItemType Directory -Force $rdir | Out-Null
        Get-ChildItem $rdir -Filter '*.msi' | Remove-Item -Force
        Copy-Item $m.FullName $rdir
        Write-Host "Release MSI for the hands-on checklist: $($m.Name)"
    } else {
        Write-Host 'No release build in dist\release (run scripts/build-release.ps1 first).' -ForegroundColor Yellow
    }
}
Get-ChildItem $box -Recurse -File | ForEach-Object { '{0,-50} {1,8:N1} MB' -f $_.FullName.Substring($box.Length + 1), ($_.Length / 1MB) }
Write-Host ''
Write-Host 'Ready. Automated test: double-click scripts\sandbox-auto.wsb (results in dist\sandbox-results).'
Write-Host 'Hands-on checklist:    double-click scripts\sandbox-test.wsb (docs/clean-machine-test.md).'
