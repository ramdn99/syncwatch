<#
.SYNOPSIS
  Two-instance room test INSIDE Windows Sandbox (Windows PowerShell 5.1), called by
  auto-test.ps1 with the installed TEST build. No mouse or keyboard input, no relay servers
  (the sandbox has no relay setup; the real-relay test runs on the developer's PC with
  scripts/dev/release-pair.ps1 + two-windows.ps1 -ReleasePair).

.DESCRIPTION
  Copies the installed program into two portable folders (each with its own data folder),
  then runs the "sync" scenario of the test build: the host creates a room with a password,
  makes an invite (file exchange), the member replies, the host admits; then play, pause,
  seek, speed, audio and subtitle tracks, external subtitle and audio files, chat-free checks;
  each window writes PASS/FAIL lines. Prints "ROOMTEST pass=<n> fail=<n>".
#>
param(
    [Parameter(Mandatory = $true)][string]$Install,
    [Parameter(Mandatory = $true)][string]$MediaDir,
    [Parameter(Mandatory = $true)][string]$Out,
    [string]$Password = 'zq-sandbox-pw',
    [int]$Minutes = 9
)
$ErrorActionPreference = 'Continue'
$base = 'C:\swroom'
Remove-Item -Recurse -Force $base -ErrorAction SilentlyContinue
$work = Join-Path $base 'work'
New-Item -ItemType Directory -Force $work | Out-Null
$procs = @()
foreach ($w in 'a', 'b') {
    $dir = Join-Path $base "$w\SyncWatch"
    New-Item -ItemType Directory -Force $dir | Out-Null
    Copy-Item (Join-Path $Install '*') $dir -Recurse -Force
    Remove-Item (Join-Path $dir 'Uninstall SyncWatch.lnk') -ErrorAction SilentlyContinue
    New-Item -ItemType File (Join-Path $dir 'portable.flag') -Force | Out-Null
}
$env:SYNCWATCH_DEV_BYPASS_CONNECTION = '1'
$env:SYNCWATCH_DEV_ICE = 'none'
$env:SYNCWATCH_HANDSHAKE = 'manual'
$env:SYNCWATCH_DEV_ROOM_FILE = Join-Path $work 'room.txt'
$env:SYNCWATCH_DEV_ROOM_PASSWORD = $Password
$env:SYNCWATCH_LOG = 'debug'
$env:SYNCWATCH_DEV_SCENARIO = 'sync'
$env:SYNCWATCH_DEV_SCENARIO_MEDIA = Join-Path $MediaDir 'long.mkv'
$env:SYNCWATCH_DEV_SCENARIO_EXTRA = (Join-Path $MediaDir 'extra-cp1252.srt') + ';' + (Join-Path $MediaDir 'extra-audio.mka')
Remove-Item Env:SYNCWATCH_TEST_AUTO_UPDATE, Env:SYNCWATCH_DEV_UPDATE_URL -ErrorAction SilentlyContinue
foreach ($w in 'a', 'b') {
    $env:SYNCWATCH_DEV_IDENTITY = $w
    $env:SYNCWATCH_DEV_NAME = $(if ($w -eq 'a') { 'Host A' } else { 'Member B' })
    $env:SYNCWATCH_DEV_AUTOROOM = $(if ($w -eq 'a') { 'host' } else { 'join' })
    $env:SYNCWATCH_DEV_SCENARIO_ROLE = ''
    $env:SYNCWATCH_DEV_SCENARIO_FILE = Join-Path $work "scenario-$w.txt"
    $procs += Start-Process (Join-Path $base "$w\SyncWatch\syncwatch.exe") -PassThru
    Start-Sleep -Seconds 3
}
$end = (Get-Date).AddMinutes($Minutes)
while ((Get-Date) -lt $end) {
    $done = @('a', 'b' | Where-Object { $f = Join-Path $work "scenario-$_.txt"; (Test-Path $f) -and ((Get-Content $f) -contains 'DONE') })
    if ($done.Count -eq 2) { break }
    Start-Sleep -Seconds 2
}
foreach ($p in $procs) { if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } }
Start-Sleep -Seconds 2
$pass = 0; $fail = 0
New-Item -ItemType Directory -Force $Out | Out-Null
foreach ($w in 'a', 'b') {
    $f = Join-Path $work "scenario-$w.txt"
    if (Test-Path $f) {
        Copy-Item $f (Join-Path $Out "room-scenario-$w.txt")
        $lines = Get-Content $f
        $pass += @($lines | Where-Object { $_ -like 'PASS*' }).Count
        $fail += @($lines | Where-Object { $_ -like 'FAIL*' }).Count
    } else {
        $fail++
    }
    Copy-Item (Join-Path $base "$w\SyncWatch\data\logs") (Join-Path $Out "room-logs-$w") -Recurse -ErrorAction SilentlyContinue
}
Remove-Item -Recurse -Force $base -ErrorAction SilentlyContinue
"ROOMTEST pass=$pass fail=$fail"
