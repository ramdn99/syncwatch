#Requires -Version 7.0
<#
.SYNOPSIS
  Two SyncWatch windows on this PC, as two different people ('a' = host, 'b' = member), with a
  local signaling server and the dev UI server. Stops everything it started when done.
  Never moves the mouse or presses keys.

.DESCRIPTION
  Sets SYNCWATCH_SIGNALING_URL and SYNCWATCH_DEV_IDENTITY (a / b) for each instance, starts the
  signaling server (local binary, or Docker with -Docker), the Vite dev server on port 1420 and
  both app instances. With -AutoRoom, 'a' creates a room and 'b' joins it automatically.
  With -Soak N both join automatically, 'b' is minimized, and after N seconds the script checks
  that no "Reconnecting" banner and no link reconnect appeared (spec section 2, pitfall 2).

  If PowerShell refuses to run scripts, call it through pwsh as below. If pnpm is blocked,
  the script uses pnpm.cmd.

.PARAMETER Ice
  none    direct only, no relay (default; no quota)
  coturn  relay-only through the local coturn container (Docker Desktop must be running)
  metered your saved Metered connection (uses quota)

.EXAMPLE
  pwsh -NoProfile -File scripts/dev/two-windows.ps1
.EXAMPLE
  pwsh -NoProfile -File scripts/dev/two-windows.ps1 -AutoRoom -Password test
.EXAMPLE
  pwsh -NoProfile -File scripts/dev/two-windows.ps1 -Soak 900
#>
[CmdletBinding()]
param(
    [ValidateSet('none', 'coturn', 'metered')][string]$Ice = 'none',
    [switch]$AutoRoom,
    [string]$Password = '',
    [int]$Soak = 0,
    # Scripted end-to-end check between the two windows (debug builds): 'chat'.
    [ValidateSet('', 'chat', 'rooms')][string]$Scenario = '',
    [switch]$Docker,
    # Stop a stale Vite/node process of THIS repo that still holds port 1420.
    [switch]$FreePort,
    [string]$Media = ''
)

if ($PSVersionTable.PSVersion.Major -lt 7) { Write-Host 'Run with PowerShell 7: pwsh -NoProfile -File scripts/dev/two-windows.ps1'; exit 2 }
$ErrorActionPreference = 'Stop'
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
Set-Location $root
if (Test-Path 'D:\Rust\cargo\bin') { $env:Path = "D:\Rust\cargo\bin;$env:Path" }
if (-not $env:RUSTUP_HOME) { $env:RUSTUP_HOME = 'D:\Rust\rustup'; $env:CARGO_HOME = 'D:\Rust\cargo' }
if (-not $env:LIBMPV_DIR) { $env:LIBMPV_DIR = 'D:\libs\mpv' }
if ($Soak -gt 0 -or $Scenario) { $AutoRoom = $true }

function Port-Busy([int]$port) {
    $c = [Net.Sockets.TcpClient]::new()
    try { $c.ConnectAsync('127.0.0.1', $port).Wait(300) -and $c.Connected } catch { $false } finally { $c.Dispose() }
}
function Port-Owner([int]$port) {
    $conn = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($conn) { Get-Process -Id $conn.OwningProcess -ErrorAction SilentlyContinue }
}

foreach ($port in 1420, 8787) {
    if (-not (Port-Busy $port)) { continue }
    $owner = Port-Owner $port
    $desc = if ($owner) { "$($owner.ProcessName) (PID $($owner.Id))" } else { 'an unknown process' }
    if ($port -eq 1420 -and $FreePort -and $owner -and $owner.ProcessName -eq 'node') {
        Write-Host "Port 1420 is held by $desc; stopping it (-FreePort)."
        & taskkill /PID $owner.Id /T /F *> $null
        Start-Sleep -Seconds 1
        if (Port-Busy 1420) { Write-Host 'Port 1420 is still busy.' -ForegroundColor Red; exit 1 }
    } else {
        $hint = if ($port -eq 1420) { ' Close the other dev server, or re-run with -FreePort to stop a stale node process.' } else { ' Stop the other signaling server first.' }
        Write-Host "Port $port is busy: used by $desc.$hint" -ForegroundColor Red
        exit 1
    }
}

Write-Host 'Building...'
cargo build -q -p syncwatch-desktop $(if (-not $Docker) { '-p'; 'syncwatch-signaling' })
if ($LASTEXITCODE) { exit 1 }

$work = Join-Path ([IO.Path]::GetTempPath()) "syncwatch-two-windows-$PID"
New-Item -ItemType Directory -Force $work | Out-Null
$started = @()
$compose = @()
try {
    if ($Docker) {
        docker compose -f deploy/compose.dev.yml up -d --build signaling | Out-Null
        if ($LASTEXITCODE) { throw 'Could not start the signaling container (is Docker Desktop running?)' }
        $compose += 'signaling'
    } else {
        $env:SYNCWATCH_SIGNALING_BIND = '127.0.0.1:8787'
        $started += Start-Process target\debug\syncwatch-signaling.exe -PassThru -WindowStyle Hidden -RedirectStandardOutput "$work\signaling.log" -RedirectStandardError "$work\signaling.err"
    }
    if ($Ice -eq 'coturn') {
        docker compose -f deploy/compose.dev.yml up -d coturn | Out-Null
        if ($LASTEXITCODE) { throw 'Could not start coturn (is Docker Desktop running?)' }
        $compose += 'coturn'
    }
    $pnpm = (Get-Command pnpm.cmd -ErrorAction SilentlyContinue)?.Source ?? "$env:APPDATA\npm\pnpm.cmd"
    $started += Start-Process $pnpm -ArgumentList '--dir', 'apps/desktop', 'dev' -PassThru -WindowStyle Hidden -RedirectStandardOutput "$work\vite.log" -RedirectStandardError "$work\vite.err"
    $deadline = (Get-Date).AddMinutes(2)
    while (-not (Port-Busy 1420) -or -not (Port-Busy 8787)) {
        if ((Get-Date) -gt $deadline) { throw 'The dev UI server or the signaling server did not start.' }
        Start-Sleep -Milliseconds 300
    }

    $env:SYNCWATCH_SIGNALING_URL = 'ws://127.0.0.1:8787/ws'
    $env:SYNCWATCH_DEV_BYPASS_CONNECTION = '1'
    $env:SYNCWATCH_DEV_ICE = $(if ($Ice -eq 'metered') { '' } else { $Ice })
    # -Ice metered: relay-only through your Metered account (keep such runs short: quota).
    $env:SYNCWATCH_DEV_FORCE_RELAY = $(if ($Ice -eq 'metered') { '1' } else { '' })
    $env:SYNCWATCH_DEV_ROOM_FILE = Join-Path $work 'room.txt'
    $env:SYNCWATCH_DEV_ROOM_PASSWORD = $Password
    # Startup media applies only to the first launch of a window (never to rooms; entering a
    # room unloads the player). The rooms scenario uses its own variable instead.
    if ($Media -and $Scenario -ne 'rooms') { $env:SYNCWATCH_DEV_MEDIA = $Media } else { Remove-Item env:SYNCWATCH_DEV_MEDIA -ErrorAction SilentlyContinue }
    $apps = @()
    foreach ($who in 'a', 'b') {
        $env:SYNCWATCH_DEV_IDENTITY = $who
        $env:SYNCWATCH_DEV_NAME = $(if ($who -eq 'a') { 'Host A' } else { 'Member B' })
        $env:SYNCWATCH_DEV_AUTOROOM = $(if ($AutoRoom) { if ($who -eq 'a') { 'host' } else { 'join' } } else { '' })
        $env:SYNCWATCH_DEV_SOAK_FILE = $(if ($Soak -gt 0) { Join-Path $work "soak-$who.json" } else { '' })
        $env:SYNCWATCH_DEV_MINIMIZE = $(if ($Soak -gt 0 -and $who -eq 'b') { '1' } else { '' })
        $env:SYNCWATCH_DEV_SCENARIO = $Scenario
        # The rooms scenario loads this video inside a room (read only). It is NOT the startup
        # media variable, so nothing is loaded before a room picks it.
        $env:SYNCWATCH_DEV_SCENARIO_MEDIA = $(if ($Scenario -eq 'rooms') { if ($Media) { $Media } else { 'D:\media\test.mkv' } } else { '' })
        $env:SYNCWATCH_DEV_SCENARIO_FILE = $(if ($Scenario) { Join-Path $work "scenario-$who.txt" } else { '' })
        $p = Start-Process target\debug\syncwatch.exe -PassThru
        $started += $p
        $apps += $p
        Write-Host "Window $who started (PID $($p.Id), data folder %LOCALAPPDATA%\SyncWatch-dev-$who)."
        Start-Sleep -Seconds 3
    }

    if ($Scenario) {
        Write-Host "Scenario '$Scenario' running in both windows (no mouse or keyboard input)..."
        $end = (Get-Date).AddMinutes(4)
        while ((Get-Date) -lt $end) {
            $done = @('a', 'b') | Where-Object { $p = Join-Path $work "scenario-$_.txt"; (Test-Path $p) -and ((Get-Content $p) -contains 'DONE') }
            if ($done.Count -eq 2) { break }
            Start-Sleep -Seconds 1
        }
    } elseif ($Soak -gt 0) {
        Write-Host "Soak test: $Soak s with window b minimized. Do not close the windows."
        $end = (Get-Date).AddSeconds($Soak + 15)
        while ((Get-Date) -lt $end -and -not ($apps | Where-Object HasExited)) { Start-Sleep -Seconds 5 }
    } else {
        Write-Host 'Both windows are running. Close both windows (or press Ctrl+C here) to stop everything.'
        while ($apps | Where-Object { -not $_.HasExited }) { Start-Sleep -Seconds 1 }
    }
}
finally {
    foreach ($s in $started) { if (-not $s.HasExited) { & taskkill /PID $s.Id /T /F *> $null } }
    foreach ($svc in $compose) { docker compose -f deploy/compose.dev.yml rm -sf $svc *> $null }
    Start-Sleep -Seconds 1
    foreach ($port in 1420, 8787) { if (Port-Busy $port) { Write-Host "WARN: port $port is still in use." -ForegroundColor Yellow } }
}

if ($Scenario) {
    $fail = $false
    foreach ($who in 'a', 'b') {
        $p = Join-Path $work "scenario-$who.txt"
        Write-Host "=== window $who"
        if (-not (Test-Path $p)) { Write-Host 'no result' -ForegroundColor Red; $fail = $true; continue }
        $lines = Get-Content $p
        foreach ($l in $lines) {
            if ($l -like 'PASS*') { Write-Host $l -ForegroundColor Green } elseif ($l -like 'FAIL*') { Write-Host $l -ForegroundColor Red; $fail = $true } else { Write-Host $l }
        }
        if ($lines -notcontains 'DONE') { Write-Host 'did not finish' -ForegroundColor Red; $fail = $true }
    }
    if ($fail) { exit 1 }
}

if ($Soak -gt 0) {
    $fail = $false
    foreach ($who in 'a', 'b') {
        $f = Join-Path $work "soak-$who.json"
        if (-not (Test-Path $f)) { Write-Host "soak $who`: no result" -ForegroundColor Red; $fail = $true; continue }
        $r = Get-Content $f -Raw | ConvertFrom-Json
        $ok = $r.inRoom -and $r.reconnectingBanners -eq 0 -and $r.linkReconnecting -eq 0 -and $r.elapsedS -ge $Soak
        $line = "soak $who`: elapsed $($r.elapsedS) s, in room $($r.inRoom), members $($r.members), banners $($r.reconnectingBanners), link reconnects $($r.linkReconnecting), links $($r.links -join ',')"
        if ($ok) { Write-Host "PASS $line" -ForegroundColor Green } else { Write-Host "FAIL $line" -ForegroundColor Red; $fail = $true }
    }
    Write-Host "Logs: %LOCALAPPDATA%\SyncWatch-dev-a\logs and %LOCALAPPDATA%\SyncWatch-dev-b\logs; results in $work"
    if ($fail) { exit 1 }
}
