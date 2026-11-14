#Requires -Version 7.0
<#
.SYNOPSIS
  Two (or three) SyncWatch windows on this PC as different people ('a' = host, 'b' = member,
  'c' = late joiner), with a local signaling server and the dev UI server. Stops everything it
  started when done. Never moves the mouse or presses keys.

.DESCRIPTION
  Sets SYNCWATCH_SIGNALING_URL and SYNCWATCH_DEV_IDENTITY (a / b / c) for each instance, starts
  the signaling server (local binary, or Docker with -Docker), the Vite dev server on port 1420
  and the app instances. With -AutoRoom, 'a' creates a room and the others join automatically.
  With -Soak N both join automatically, 'b' is minimized, and after N seconds the script checks
  that no "Reconnecting" banner and no link reconnect appeared (spec section 2, pitfall 2).
  With -Scenario X each window runs a scripted check through the real UI code path and the
  script prints PASS/FAIL, cross-window latencies, position agreement and a log grep.

  If PowerShell refuses to run scripts, call it through pwsh as below. If pnpm is blocked,
  the script uses pnpm.cmd. Scenarios sync/chattime/late need the test media:
  pwsh -NoProfile -File scripts/dev/make-test-media.ps1

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
.EXAMPLE
  pwsh -NoProfile -File scripts/dev/two-windows.ps1 -Scenario sync
#>
[CmdletBinding()]
param(
    [ValidateSet('none', 'coturn', 'metered')][string]$Ice = 'none',
    [switch]$AutoRoom,
    [string]$Password = '',
    [int]$Soak = 0,
    # Scripted end-to-end checks (debug builds): chat, rooms, sync, chattime, late (3 windows).
    [ValidateSet('', 'chat', 'rooms', 'sync', 'chattime', 'late', 'popup', 'succession', 'torrent', 'skiprelay', 'torrentfile', 'torrentgate', 'torrentplay')][string]$Scenario = '',
    [switch]$Docker,
    # Stop a stale Vite/node process of THIS repo that still holds port 1420.
    [switch]$FreePort,
    [string]$Media = '',
    # Generated test media (scripts/dev/make-test-media.ps1).
    [string]$MediaDir = (Join-Path ([IO.Path]::GetTempPath()) 'syncwatch-media'),
    # App log level for the windows (scenarios default to debug so the log grep sees the most).
    [string]$LogLevel = ''
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

# Scenario media (only read; never part of the repo).
$scenarioMedia = switch ($Scenario) {
    'chattime' { Join-Path $MediaDir 'long.mkv' }
    { $_ -in 'rooms', 'sync', 'late', 'popup', 'succession', 'skiprelay' } { if ($Media) { $Media } else { 'D:\media\test.mkv' } }
    # torrent: the magnet (from scripts/dev/torrent-lab.ps1, which also runs the seeder).
    { $_ -in 'torrent', 'torrentfile', 'torrentgate', 'torrentplay' } { $env:SYNCWATCH_LAB_MAGNET }
    default { '' }
}
$extra = switch ($Scenario) {
    { $_ -in 'torrent', 'torrentgate', 'torrentplay' } { @($env:SYNCWATCH_LAB_FILE) }
    'torrentfile' { @($env:SYNCWATCH_LAB_TORRENT_FILE, 'D:\media\test.mkv') }
    'sync' { @((Join-Path $MediaDir 'extra-cp1252.srt'), (Join-Path $MediaDir 'extra-audio.mka')) }
    'late' { @('D:\media\test-mismatch.mkv') }
    'skiprelay' { @((Join-Path $MediaDir 'extra-audio.mka')) }
    default { @() }
}
foreach ($n in $(if ($Scenario -in 'torrent', 'torrentfile', 'torrentgate', 'torrentplay') { @() } else { @($scenarioMedia) + $extra })) {
    if ($n -and -not (Test-Path $n)) { Write-Host "Missing test file $n. Run: pwsh -NoProfile -File scripts/dev/make-test-media.ps1" -ForegroundColor Red; exit 1 }
}

Write-Host 'Building...'
cargo build -q -p syncwatch-desktop $(if (-not $Docker) { '-p'; 'syncwatch-signaling' })
if ($LASTEXITCODE) { exit 1 }

$work = Join-Path ([IO.Path]::GetTempPath()) "syncwatch-two-windows-$PID"
New-Item -ItemType Directory -Force $work | Out-Null
$windows = @('a', 'b')
if ($Scenario -in 'late', 'succession', 'chat', 'torrentfile') { $windows += 'c' }
# Succession: window a (the host) is killed on purpose; only b and c must finish.
$mustFinish = @($windows | Where-Object { -not ($Scenario -eq 'succession' -and $_ -eq 'a') })
$killed = $false
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
    $env:SYNCWATCH_LOG = $(if ($LogLevel) { $LogLevel } elseif ($Scenario) { 'debug' } else { '' })
    # Startup media applies only to the first launch of a window (never to rooms; entering a
    # room unloads the player). Scenarios use their own variable instead.
    if ($Media -and -not $Scenario) { $env:SYNCWATCH_DEV_MEDIA = $Media } else { Remove-Item env:SYNCWATCH_DEV_MEDIA -ErrorAction SilentlyContinue }
    $env:SYNCWATCH_DEV_SCENARIO = $Scenario
    $env:SYNCWATCH_DEV_SCENARIO_MEDIA = $scenarioMedia
    $env:SYNCWATCH_DEV_SCENARIO_EXTRA = $extra -join ';'
    $apps = @()
    foreach ($who in $windows) {
        if ($who -eq 'c' -and $Scenario -eq 'late') {
            Write-Host 'Window c joins late (in 25 s)...'
            Start-Sleep -Seconds 25
        }
        $env:SYNCWATCH_DEV_IDENTITY = $who
        $env:SYNCWATCH_DEV_NAME = switch ($who) { 'a' { 'Host A' } 'b' { 'Member B' } default { 'Late C' } }
        $env:SYNCWATCH_DEV_AUTOROOM = $(if ($AutoRoom) { if ($who -eq 'a') { 'host' } elseif ($who -eq 'c' -and $Scenario -eq 'chat') { 'manual' } else { 'join' } } else { '' })
        $env:SYNCWATCH_DEV_SCENARIO_ROLE = $(if ($who -eq 'c' -and $Scenario -in 'late', 'chat', 'torrentfile') { 'late' } else { '' })
        $env:SYNCWATCH_DEV_SOAK_FILE = $(if ($Soak -gt 0) { Join-Path $work "soak-$who.json" } else { '' })
        $env:SYNCWATCH_DEV_MINIMIZE = $(if ($Soak -gt 0 -and $who -eq 'b') { '1' } else { '' })
        $env:SYNCWATCH_DEV_SCENARIO_FILE = $(if ($Scenario) { Join-Path $work "scenario-$who.txt" } else { '' })
        $p = Start-Process target\debug\syncwatch.exe -PassThru
        $started += $p
        $apps += $p
        Write-Host "Window $who started (PID $($p.Id), data folder %LOCALAPPDATA%\SyncWatch-dev-$who)."
        Start-Sleep -Seconds 3
    }

    if ($Scenario) {
        Write-Host "Scenario '$Scenario' running in $($windows.Count) windows (no mouse or keyboard input)..."
        $end = (Get-Date).AddMinutes(9)
        while ((Get-Date) -lt $end) {
            if ($Scenario -eq 'succession' -and -not $killed) {
                $fa = Join-Path $work 'scenario-a.txt'
                if ((Test-Path $fa) -and ((Get-Content $fa) -contains 'KILL-HOST')) {
                    Write-Host 'Killing the host window (a real crash: no clean leave)...'
                    & taskkill /PID $apps[0].Id /T /F *> $null
                    $killed = $true
                }
            }
            $done = @($mustFinish | Where-Object { $f = Join-Path $work "scenario-$_.txt"; (Test-Path $f) -and ((Get-Content $f) -contains 'DONE') })
            if ($done.Count -eq $mustFinish.Count) { break }
            Start-Sleep -Seconds 1
        }
    } elseif ($Soak -gt 0) {
        Write-Host "Soak test: $Soak s with window b minimized. Do not close the windows."
        $end = (Get-Date).AddSeconds($Soak + 15)
        while ((Get-Date) -lt $end -and -not ($apps | Where-Object HasExited)) { Start-Sleep -Seconds 5 }
    } else {
        Write-Host 'The windows are running. Close them (or press Ctrl+C here) to stop everything.'
        while ($apps | Where-Object { -not $_.HasExited }) { Start-Sleep -Seconds 1 }
    }
}
finally {
    foreach ($s in $started) { if (-not $s.HasExited) { & taskkill /PID $s.Id /T /F *> $null } }
    foreach ($svc in $compose) { docker compose -f deploy/compose.dev.yml rm -sf $svc *> $null }
    Start-Sleep -Seconds 1
    foreach ($port in 1420, 8787) { if (Port-Busy $port) { Write-Host "WARN: port $port is still in use." -ForegroundColor Yellow } }
}

function Show-Analysis($windows, $all) {
    # Cross-window latency (same PC clock): host "TS x_sent" -> member "TS x_seen".
    $sent = @{}; $seen = @{}
    foreach ($l in $all['a']) { if ($l -match '^TS (\w+)_sent (\d+)') { $sent[$Matches[1]] = [long]$Matches[2] } }
    foreach ($l in $all['b']) { if ($l -match '^TS (\w+)_seen (\d+)') { $seen[$Matches[1]] = [long]$Matches[2] } }
    foreach ($k in $sent.Keys) {
        if ($seen.ContainsKey($k)) { Write-Host ('LATENCY {0,-8} {1,5} ms (host action until visible in the member player, incl. 100 ms polling)' -f $k, ($seen[$k] - $sent[$k])) }
    }
    # Position agreement: each window's samples against the host's, paired by wall time.
    $pos = @{}
    foreach ($who in $windows) {
        $pos[$who] = @($all[$who] | Where-Object { $_ -match '^POS ' } | ForEach-Object {
                $x = $_ -split ' '
                [pscustomobject]@{ tag = $x[1]; t = [long]$x[2]; p = [long]$x[3]; paused = ($x[4] -eq '1') }
            })
    }
    # The reference for a tag is the first window that sampled it (the host, or after a
    # succession the new host); every other window is compared with it.
    $tags = @($windows | ForEach-Object { $pos[$_].tag } | Select-Object -Unique)
    foreach ($tag in $tags) {
        $have = @($windows | Where-Object { @($pos[$_] | Where-Object tag -EQ $tag).Count -gt 0 })
        if ($have.Count -lt 2) { continue }
        $ref = $have[0]
        foreach ($who in ($have | Select-Object -Skip 1)) {
            $worst = 0; $n = 0
            foreach ($s in ($pos[$who] | Where-Object tag -EQ $tag)) {
                $h = $pos[$ref] | Where-Object tag -EQ $tag | Sort-Object { [math]::Abs($_.t - $s.t) } | Select-Object -First 1
                if (-not $h -or [math]::Abs($h.t - $s.t) -gt 400) { continue }
                $expected = $h.p
                if (-not $h.paused) { $expected += ($s.t - $h.t) }
                $d = [math]::Abs($s.p - $expected); $n++
                if ($d -gt $worst) { $worst = $d }
            }
            if ($n -gt 0) { Write-Host ('POSITION {4} vs {0} [{1}]: worst difference {2} ms over {3} paired samples' -f $who, $tag, $worst, $n, $ref) }
        }
    }
    foreach ($who in $windows) { foreach ($l in $all[$who]) { if ($l -match '^(CHIP|ORDER|CHIPS|INFO) ') { Write-Host "$who $l" } } }
}

function Test-Logs($windows) {
    # No chat content (every scenario chat text carries the marker "zqx") and no credential
    # values in any log of these windows. Values are compared, never printed.
    $logs = @(foreach ($who in $windows) { Get-ChildItem "$env:LOCALAPPDATA\SyncWatch-dev-$who\logs" -File -ErrorAction SilentlyContinue })
    $text = ($logs | ForEach-Object { Get-Content $_.FullName -Raw }) -join "`n"
    $chatHits = ([regex]::Matches($text, 'zqx')).Count
    $vals = @()
    foreach ($ef in '.env.local', '.env.ice.local') {
        if (-not (Test-Path $ef)) { continue }
        $t = Get-Content $ef -Raw
        $vals += [regex]::Matches($t, '"?(username|credential)"?\s*:\s*"([^"]+)"') | ForEach-Object { $_.Groups[2].Value }
        $vals += [regex]::Matches($t, '(?m)^\s*METERED_(APP|API_KEY)\s*=\s*"?([^"\r\n]+)') | ForEach-Object { $_.Groups[2].Value.Trim() }
    }
    $secretHits = @($vals | Where-Object { $_.Length -ge 6 -and $text.Contains($_) }).Count
    if ($chatHits -eq 0 -and $secretHits -eq 0) {
        Write-Host "PASS log grep: $($logs.Count) log files, no chat content, no credential values ($($vals.Count) checked)" -ForegroundColor Green
        return $true
    }
    Write-Host "FAIL log grep: chat marker hits $chatHits, credential hits $secretHits" -ForegroundColor Red
    return $false
}

if ($Scenario) {
    $fail = $false
    $all = @{}
    foreach ($who in $windows) {
        $f = Join-Path $work "scenario-$who.txt"
        Write-Host "=== window $who"
        if (-not (Test-Path $f)) { Write-Host 'no result' -ForegroundColor Red; $fail = $true; $all[$who] = @(); continue }
        $lines = @(Get-Content $f)
        $all[$who] = $lines
        foreach ($l in $lines) {
            if ($l -like 'PASS*') { Write-Host $l -ForegroundColor Green }
            elseif ($l -like 'FAIL*') { Write-Host $l -ForegroundColor Red; $fail = $true }
            elseif ($l -notmatch '^(TS|POS|CHIP|ORDER|CHIPS|INFO) ') { Write-Host $l }
        }
        if ($lines -notcontains 'DONE' -and $who -in $mustFinish) { Write-Host 'did not finish' -ForegroundColor Red; $fail = $true }
    }
    Show-Analysis $windows $all
    if (-not (Test-Logs $windows)) { $fail = $true }
    Write-Host "Raw results: $work"
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
