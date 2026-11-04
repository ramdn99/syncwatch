#Requires -Version 7.0
<#
.SYNOPSIS
  Network lab: two SyncWatch instances connect through the real signaling server and the real
  WebView2 bridge, and report measurements (connect time, RTT, throughput, keepalive gaps,
  watchdog behavior). Does NOT move the mouse or press keys.

.PARAMETER Ice
  none    host candidates only (direct, local)
  coturn  relay-only through the local coturn container (deploy/compose.dev.yml)
  metered relay-only through YOUR Metered account (uses quota: keep -Seconds and -MB small)

.EXAMPLE
  pwsh -NoProfile -File scripts/dev/net-lab.ps1 -Ice none -Seconds 30 -MB 64
.EXAMPLE
  pwsh -NoProfile -File scripts/dev/net-lab.ps1 -Ice coturn -Seconds 780 -MB 16 -Minimized
.EXAMPLE
  pwsh -NoProfile -File scripts/dev/net-lab.ps1 -Ice none -Seconds 60 -MB 0 -DropAt 15
#>
[CmdletBinding()]
param(
    [ValidateSet('none', 'coturn', 'metered')][string]$Ice = 'none',
    [int]$Seconds = 30,
    [int]$MB = 64,
    [int]$DropAt = 0,
    [int]$FreezeAt = 0,
    [switch]$Minimized,
    # Optimized build (debug assertions kept on, so the dev-only lab code is compiled in).
    [switch]$Release,
    # In-page loopback data-channel benchmark size (host only; 0 = skip).
    [int]$BenchMB = 0
)

if ($PSVersionTable.PSVersion.Major -lt 7) { Write-Host 'Run with PowerShell 7: pwsh -NoProfile -File scripts/dev/net-lab.ps1'; exit 2 }
$ErrorActionPreference = 'Stop'
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
Set-Location $root
foreach ($p in 'D:\Rust\cargo\bin') { if (Test-Path $p) { $env:Path = "$p;$env:Path" } }
if (-not $env:RUSTUP_HOME) { $env:RUSTUP_HOME = 'D:\Rust\rustup'; $env:CARGO_HOME = 'D:\Rust\cargo' }
if (-not $env:LIBMPV_DIR) { $env:LIBMPV_DIR = 'D:\libs\mpv' }

function Port-Busy([int]$port) {
    # A real connect attempt (Get-NetTCPConnection does not always list node's listener).
    $c = [Net.Sockets.TcpClient]::new()
    try { $c.ConnectAsync('127.0.0.1', $port).Wait(300) -and $c.Connected } catch { $false } finally { $c.Dispose() }
}
foreach ($port in 1420, 8787) {
    if (Port-Busy $port) { Write-Host "Port $port is busy. Close whatever is using it first." -ForegroundColor Red; exit 1 }
}

Write-Host 'Building signaling server and desktop app...'
$profileDir = if ($Release) { $env:CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS = 'true'; 'release' } else { 'debug' }
cargo build -q -p syncwatch-signaling -p syncwatch-desktop $(if ($Release) { '--release' })
if ($LASTEXITCODE) { exit 1 }

$lab = Join-Path ([IO.Path]::GetTempPath()) "syncwatch-netlab-$PID"
New-Item -ItemType Directory -Force $lab | Out-Null
$started = @()
$composeUp = $false
try {
    if ($Ice -eq 'coturn') {
        docker compose -f deploy/compose.dev.yml up -d coturn | Out-Null
        if ($LASTEXITCODE) { throw 'Could not start coturn (is Docker Desktop running?)' }
        $composeUp = $true
    }
    $env:SYNCWATCH_SIGNALING_BIND = '127.0.0.1:8787'
    $started += Start-Process target\$profileDir\syncwatch-signaling.exe -PassThru -WindowStyle Hidden -RedirectStandardOutput "$lab\signaling.log" -RedirectStandardError "$lab\signaling.err"
    $pnpm = (Get-Command pnpm.cmd -ErrorAction SilentlyContinue)?.Source ?? "$env:APPDATA\npm\pnpm.cmd"
    $started += Start-Process $pnpm -ArgumentList '--dir', 'apps/desktop', 'dev' -PassThru -WindowStyle Hidden
    $deadline = (Get-Date).AddMinutes(1)
    while (-not (Port-Busy 1420) -or -not (Port-Busy 8787)) {
        if ((Get-Date) -gt $deadline) { throw 'Dev server or signaling server did not start.' }
        Start-Sleep -Milliseconds 300
    }

    $common = @{
        SYNCWATCH_SIGNALING_URL = 'ws://127.0.0.1:8787/ws'
        SYNCWATCH_NET_LAB_DIR = $lab
        SYNCWATCH_NET_LAB_ICE = $Ice
        SYNCWATCH_NET_LAB_SECONDS = "$Seconds"
        SYNCWATCH_NET_LAB_MB = "$MB"
        SYNCWATCH_NET_LAB_MINIMIZE = $(if ($Minimized) { '1' } else { '0' })
        SYNCWATCH_LOG = 'info'
        SYNCWATCH_NET_LAB_BENCH_MB = "$BenchMB"
        # Metered mode reads the static ICE list from the git-ignored file (values never printed).
        SYNCWATCH_NET_LAB_ICE_FILE = (Join-Path $root '.env.ice.local')
    }
    foreach ($role in 'host', 'join') {
        foreach ($k in $common.Keys) { Set-Item "env:$k" $common[$k] }
        $env:SYNCWATCH_NET_LAB = $role
        $env:SYNCWATCH_DEV_IDENTITY = $(if ($role -eq 'host') { 'a' } else { 'b' })
        $env:SYNCWATCH_NET_LAB_DROP_AT = $(if ($role -eq 'join' -and $DropAt -gt 0) { "$DropAt" } else { '' })
        $env:SYNCWATCH_NET_LAB_FREEZE_AT = $(if ($role -eq 'join' -and $FreezeAt -gt 0) { "$FreezeAt" } else { '' })
        $started += Start-Process target\$profileDir\syncwatch.exe -PassThru -WindowStyle $(if ($Minimized) { 'Minimized' } else { 'Normal' })
        if ($role -eq 'host') { Start-Sleep -Seconds 4 }
    }
    Write-Host "Lab running ($Ice, $Seconds s). Results in $lab"
    $end = (Get-Date).AddSeconds($Seconds + 90)
    while ((Get-Date) -lt $end) {
        $done = @('host', 'member') | Where-Object {
            $f = Join-Path $lab "result-$_.json"
            (Test-Path $f) -and ((Get-Content $f -Raw | ConvertFrom-Json).done)
        }
        if ($done.Count -eq 2) { break }
        Start-Sleep -Seconds 2
    }
}
finally {
    foreach ($s in $started) { if (-not $s.HasExited) { & taskkill /PID $s.Id /T /F *> $null } }
    Get-Process syncwatch -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$root*" } | Stop-Process -Force -ErrorAction SilentlyContinue
    if ($composeUp) {
        docker compose -f deploy/compose.dev.yml logs coturn *> "$lab\coturn.log"
        docker compose -f deploy/compose.dev.yml down *> $null
    }
    Start-Sleep -Seconds 1
    foreach ($port in 1420, 8787) { if (Port-Busy $port) { Write-Host "WARN port $port still in use" -ForegroundColor Yellow } }
}

foreach ($r in 'host', 'member') {
    $f = Join-Path $lab "result-$r.json"
    if (Test-Path $f) { Write-Host "=== $r"; Get-Content $f -Raw } else { Write-Host "=== ${r}: no result" -ForegroundColor Red }
}
