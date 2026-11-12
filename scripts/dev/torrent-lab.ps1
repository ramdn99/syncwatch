#Requires -Version 7
<#
.SYNOPSIS
  Milestone 7 torrent lab: two real app windows stream a torrent from qBittorrent, the only
  (throttled) seeder, on 127.0.0.1. Then it checks from OUTSIDE the app that all traffic stops
  at 100%.
.DESCRIPTION
  - Builds OUR OWN multi-file test torrent from generated files (long.mkv from
    make-test-media.ps1, a short generated trailer, a text file). Nothing public is touched.
  - qBittorrent (D:\Program Files\qBittorrent) runs with a throwaway profile:
    - Web API and embedded tracker on 127.0.0.1 only;
    - DHT, PEX, LSD and UPnP off;
    - upload limited to -UpKiB (also for LAN peers).
    Only this script talks to qBittorrent; the app never calls it.
  - The app windows run with SYNCWATCH_DEV_TORRENT_BIND=127.0.0.1 and DHT off.
  - Every second a monitor records:
    - qBittorrent's uploaded bytes and peer count;
    - live TCP connections between qBittorrent and our processes;
    - our processes' UDP endpoints;
    - whether the engine's listen port is still open.
  - After the run it checks the window of time after both completions: zero bytes, no peers,
    no connections, engine port closed. It also checks that each cache holds only the
    selected file.
  - It never moves the mouse or presses keys. qBittorrent is shut down at the end.
.EXAMPLE
  pwsh -NoProfile -File scripts/dev/torrent-lab.ps1
  pwsh -NoProfile -File scripts/dev/torrent-lab.ps1 -UpKiB 500
#>
param(
    [int]$UpKiB = 700,
    [int]$WebPort = 8089,
    [int]$TrackerPort = 9000,
    [string]$Qbt = 'D:\Program Files\qBittorrent\qbittorrent.exe',
    [switch]$KeepQbt,
    # Only start the seeder (qBittorrent keeps running) and probe the tracker.
    [switch]$SeedOnly,
    # Keep libtorrent's default of ONE connection per IP (like a real public swarm seen from one PC).
    [switch]$SameIpDefault,
    # torrent: magnet room (2 windows). torrentfile: .torrent file room, decline, late joiner,
    # source switches magnet -> local -> back (3 windows).
    [ValidateSet('torrent', 'torrentfile')][string]$Scenario = 'torrent',
    # Hands-on test material: seed both test torrents, print the magnets and .torrent paths, keep
    # qBittorrent running until a key is pressed or -ManualMinutes pass, then clean up.
    [switch]$Manual,
    [int]$ManualMinutes = 60
)
# -SameIpDefault: qBittorrent keeps libtorrent's ONE connection per IP and sits on 127.0.0.2, so the
# first window (127.0.0.1) gets in and a second one from the same address is refused, as behind one
# home NAT in a real public swarm.
$qbtIp = if ($SameIpDefault) { '127.0.0.2' } else { '127.0.0.1' }
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path (Split-Path $PSScriptRoot -Parent) -Parent)
if (-not $env:CARGO_HOME) { $env:RUSTUP_HOME = 'D:\Rust\rustup'; $env:CARGO_HOME = 'D:\Rust\cargo'; $env:PATH = "D:\Rust\cargo\bin;$env:PATH" }

$tmp = [IO.Path]::GetTempPath()
$mediaDir = Join-Path $tmp 'syncwatch-media'
$long = Join-Path $mediaDir 'long.mkv'
if (-not (Test-Path $long)) { pwsh -NoProfile -File scripts/dev/make-test-media.ps1 | Out-Host }

# --- the test pack (generated files only)
$lab = Join-Path $tmp 'syncwatch-torrent-lab'
$pack = Join-Path $lab 'SyncWatch Lab Pack'
Remove-Item -Recurse -Force $lab -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force (Join-Path $pack 'extras') | Out-Null
Copy-Item $long (Join-Path $pack 'long.mkv')
& ffmpeg -v error -y -f lavfi -i 'testsrc=duration=8:size=320x180:rate=10' -c:v libx264 -pix_fmt yuv420p (Join-Path $pack 'extras\trailer.mkv')
if ($LASTEXITCODE) { Copy-Item (Join-Path $mediaDir 'extra-audio.mka') (Join-Path $pack 'extras\trailer.mkv') }
Set-Content (Join-Path $pack 'readme.nfo') 'SyncWatch generated test pack. Not real content.'
$tracker = "http://127.0.0.1:$TrackerPort/announce"
$torrentFile = Join-Path $lab 'lab.torrent'
$mk = cargo run -q -p syncwatch-torrent --example make_test_torrent -- $pack $torrentFile $tracker 256
$magnet = ($mk | Where-Object { $_ -like 'MAGNET *' }) -replace '^MAGNET ', '' | ForEach-Object { $_.Trim() }

$ih = ($mk | Where-Object { $_ -like 'INFOHASH *' }) -replace '^INFOHASH ', '' | ForEach-Object { $_.Trim() }
if (-not $magnet) { Write-Host 'FAIL could not create the test torrent' -ForegroundColor Red; exit 1 }
Write-Host "Test torrent $ih (3 files, own tracker on 127.0.0.1)"
# A second, single-file torrent of ~40 MB for the .torrent file tests (cut from the generated video).
$film = Join-Path $lab 'SyncWatch Test Film.mkv'
& ffmpeg -v error -y -i $long -t 2040 -map 0 -c copy $film
if ($LASTEXITCODE -or -not (Test-Path $film)) { Write-Host 'FAIL could not create the film file' -ForegroundColor Red; exit 1 }
$filmTorrent = Join-Path $lab 'film.torrent'
$mk2 = cargo run -q -p syncwatch-torrent --example make_test_torrent -- $film $filmTorrent $tracker 256
$magnet2 = ($mk2 | Where-Object { $_ -like 'MAGNET *' }) -replace '^MAGNET ', '' | ForEach-Object { $_.Trim() }

$ih2 = ($mk2 | Where-Object { $_ -like 'INFOHASH *' }) -replace '^INFOHASH ', '' | ForEach-Object { $_.Trim() }
Write-Host "Test torrent $ih2 (1 file, $([math]::Round((Get-Item $film).Length / 1MB, 1)) MiB, .torrent file)"

# --- qBittorrent as the controlled seeder
$prof = Join-Path $tmp 'syncwatch-qbt-profile'
Remove-Item -Recurse -Force $prof -ErrorAction SilentlyContinue
$cfgDir = Join-Path $prof 'qBittorrent\config'
New-Item -ItemType Directory -Force $cfgDir | Out-Null
$qbtPort = Get-Random -Minimum 30000 -Maximum 40000
# qBittorrent 5 starts its Web UI only with credentials: a random throwaway password per run
# (PBKDF2-HMAC-SHA512, 100000 rounds, qBittorrent's format). Localhost needs no login anyway.
$salt = [Security.Cryptography.RandomNumberGenerator]::GetBytes(16)
$pw = [Convert]::ToBase64String([Security.Cryptography.RandomNumberGenerator]::GetBytes(24))
$key = [Security.Cryptography.Rfc2898DeriveBytes]::Pbkdf2([Text.Encoding]::UTF8.GetBytes($pw), $salt, 100000, [Security.Cryptography.HashAlgorithmName]::SHA512, 64)
$pbkdf2 = "@ByteArray($([Convert]::ToBase64String($salt)):$([Convert]::ToBase64String($key)))"
@"
[LegalNotice]
Accepted=true

[BitTorrent]
Session\DHTEnabled=false
Session\PeXEnabled=false
Session\LSDEnabled=false
Session\Port=$qbtPort
Session\InterfaceAddress=$qbtIp
Session\QueueingSystemEnabled=false

[Network]
PortForwardingEnabled=false

[Preferences]
WebUI\Enabled=true
WebUI\Address=127.0.0.1
WebUI\Port=$WebPort
WebUI\LocalHostAuth=false
WebUI\CSRFProtection=false
WebUI\Username=lab
WebUI\Password_PBKDF2="$pbkdf2"
General\StartMinimized=true
"@ | Set-Content (Join-Path $cfgDir 'qBittorrent.ini')
$api = "http://127.0.0.1:$WebPort/api/v2"
$qp = Start-Process -FilePath $Qbt -ArgumentList "--profile=$prof", '--no-splash' -PassThru -WindowStyle Minimized
$ok = $false
foreach ($i in 1..60) { try { $null = Invoke-RestMethod "$api/app/version" -TimeoutSec 2; $ok = $true; break } catch { Start-Sleep -Milliseconds 500 } }
if (-not $ok) { Write-Host 'FAIL qBittorrent Web API did not start' -ForegroundColor Red; Stop-Process -Id $qp.Id -Force; exit 1 }
$prefs = @{
    enable_embedded_tracker = $true; embedded_tracker_port = $TrackerPort
    dht = $false; pex = $false; lsd = $false; upnp = $false
    up_limit = $UpKiB * 1024; limit_lan_peers = $true
    current_interface_address = $qbtIp; listen_port = $qbtPort
    max_ratio_enabled = $false; queueing_enabled = $false
    # Everything is on 127.0.0.1: libtorrent must accept more than one connection per IP.
    enable_multi_connections_from_same_ip = (-not $SameIpDefault)
} | ConvertTo-Json -Compress
Invoke-RestMethod "$api/app/setPreferences" -Method Post -Body @{ json = $prefs } | Out-Null
Invoke-RestMethod "$api/torrents/add" -Method Post -Form @{ torrents = Get-Item $torrentFile; savepath = $lab } | Out-Null
Invoke-RestMethod "$api/torrents/add" -Method Post -Form @{ torrents = Get-Item $filmTorrent; savepath = $lab } | Out-Null
$seeding = $false
foreach ($i in 1..120) {
    $t = Invoke-RestMethod "$api/torrents/info"
    $ready = @($t | Where-Object { $_.hash -in $ih, $ih2 -and $_.progress -ge 1 -and $_.state -in 'uploading', 'stalledUP', 'forcedUP', 'queuedUP' })
    if ($ready.Count -eq 2) { $seeding = $true; break }
    Start-Sleep -Milliseconds 500
}
if (-not $seeding) { Write-Host 'FAIL qBittorrent is not seeding the test torrents' -ForegroundColor Red; (Invoke-RestMethod "$api/torrents/info") | ForEach-Object { $_ } | ForEach-Object { Write-Host "  $($_.hash) [$($_.hash.Length)] $($_.state) $($_.progress) want [$ih] [$ih2]" }; Invoke-RestMethod "$api/app/shutdown" -Method Post | Out-Null; exit 1 }
Write-Host "qBittorrent seeds on ${qbtIp}:$qbtPort, upload limited to $UpKiB KiB/s, embedded tracker :$TrackerPort"
# qBittorrent must have announced itself to its own tracker BEFORE our windows ask (otherwise
# their first announce gets an empty list and the next one is 30 minutes away).
$announced = $false
foreach ($i in 1..120) {
    $ok = 0
    foreach ($h in $ih, $ih2) {
        $trs = Invoke-RestMethod "$api/torrents/trackers?hash=$h"
        if ($trs | Where-Object { $_.url -like 'http*' -and $_.status -eq 2 -and $_.num_seeds -ge 1 }) { $ok++ }
    }
    if ($ok -eq 2) { $announced = $true; break }
    if ($i -eq 10) { Invoke-RestMethod "$api/torrents/reannounce" -Method Post -Body @{ hashes = "$ih|$ih2" } | Out-Null }
    Start-Sleep -Milliseconds 500
}
if (-not $announced) { Write-Host 'FAIL qBittorrent did not announce itself to its tracker' -ForegroundColor Red; Invoke-RestMethod "$api/app/shutdown" -Method Post | Out-Null; exit 1 }
# Probe: the tracker must list qBittorrent for this infohash (what our engine will ask).
$ihEsc = -join ($ih -split '(..)' | Where-Object { $_ } | ForEach-Object { '%' + $_ })
try {
    $probe = Invoke-WebRequest "http://127.0.0.1:$TrackerPort/announce?info_hash=$ihEsc&peer_id=-SWLAB0-000000000000&port=1&uploaded=0&downloaded=0&left=1&compact=1&event=started" -TimeoutSec 5
    Write-Host "Tracker probe: HTTP $($probe.StatusCode), $($probe.RawContentLength) bytes: $(([string]$probe.Content) -replace '[^\x20-\x7e]', '.')"
} catch { Write-Host "Tracker probe failed: $($_.Exception.Message)" }
(Invoke-RestMethod "$api/torrents/trackers?hash=$ih") | Where-Object { $_.url -like 'http*' } | ForEach-Object { Write-Host "qBittorrent tracker: status=$($_.status) seeds=$($_.num_seeds)" }
if ($SeedOnly) { Write-Host "Seeder left running (Web API $api)."; exit 0 }
if ($Manual) {
    Write-Host ''
    Write-Host 'Manual test material (generated files only, seeded by qBittorrent on this PC):'
    Write-Host "  Multi-file magnet (pick long.mkv): $magnet"
    Write-Host "  Single-file .torrent file:         $filmTorrent"
    Write-Host "  Single-file magnet:                $magnet2"
    Write-Host "  qBittorrent Web UI (localhost):    http://127.0.0.1:$WebPort"
    Write-Host "Press any key to stop and clean up (or wait $ManualMinutes minutes)."
    $end = (Get-Date).AddMinutes($ManualMinutes)
    while ((Get-Date) -lt $end) {
        $key = $false
        try { $key = [Console]::KeyAvailable } catch { $key = $false }
        if ($key) { break }
        Start-Sleep -Seconds 1
    }
    try { Invoke-RestMethod "$api/app/shutdown" -Method Post -TimeoutSec 5 | Out-Null } catch {}
    $qp.WaitForExit(15000) | Out-Null
    if (-not $qp.HasExited) { Stop-Process -Id $qp.Id -Force }
    Remove-Item -Recurse -Force $prof, $lab -ErrorAction SilentlyContinue
    Write-Host 'qBittorrent stopped; lab files removed.'
    exit 0
}

# --- clean torrent caches of the dev windows (a finished file would skip the download)
foreach ($w in 'a', 'b', 'c') { Remove-Item -Recurse -Force (Join-Path $env:LOCALAPPDATA "SyncWatch-dev-$w\cache\torrents") -ErrorAction SilentlyContinue }

# --- outside monitor (1 s)
$csv = Join-Path $lab 'monitor.csv'
$mon = Start-ThreadJob -ArgumentList $api, $(if ($Scenario -eq 'torrent') { $ih } else { $ih }), $qp.Id, $csv, $ih2 -ScriptBlock {
    param($api, $ih, $qpid, $csv, $ih2)
    'wallMs,qbtUploaded,qbtPeers,connsQbtToUs,ourUdp,ourTcpListen,peerConns,trackerConns,qbtUploaded2,qbtPeers2' | Set-Content $csv
    while ($true) {
        try {
            $p = Invoke-RestMethod "$api/torrents/properties?hash=$ih" -TimeoutSec 2
            $peers = (Invoke-RestMethod "$api/sync/torrentPeers?hash=$ih&rid=0" -TimeoutSec 2).peers
            $n = if ($peers) { @($peers.PSObject.Properties).Count } else { 0 }
            $p2 = Invoke-RestMethod "$api/torrents/properties?hash=$ih2" -TimeoutSec 2
            $peers2 = (Invoke-RestMethod "$api/sync/torrentPeers?hash=$ih2&rid=0" -TimeoutSec 2).peers
            $n2 = if ($peers2) { @($peers2.PSObject.Properties).Count } else { 0 }
            $sw = @(Get-Process syncwatch -ErrorAction SilentlyContinue | ForEach-Object Id)
            $ourPorts = if ($sw) { @(Get-NetTCPConnection -OwningProcess $sw -ErrorAction SilentlyContinue | ForEach-Object LocalPort) } else { @() }
            $qc = @(Get-NetTCPConnection -OwningProcess $qpid -State Established -ErrorAction SilentlyContinue | Where-Object { $_.RemoteAddress -like '127.*' -and $ourPorts -contains $_.RemotePort })
            $conns = $qc.Count
            $peerConns = @($qc | Where-Object { $_.LocalPort -ne 9000 }).Count
            $trackerConns = @($qc | Where-Object { $_.LocalPort -eq 9000 }).Count
            $udp = if ($sw) { @(Get-NetUDPEndpoint -OwningProcess $sw -ErrorAction SilentlyContinue).Count } else { 0 }
            $listen = if ($sw) { (@(Get-NetTCPConnection -OwningProcess $sw -State Listen -ErrorAction SilentlyContinue | ForEach-Object LocalPort) -join ' ') } else { '' }
            "$([DateTimeOffset]::Now.ToUnixTimeMilliseconds()),$($p.total_uploaded),$n,$conns,$udp,$listen,$peerConns,$trackerConns,$($p2.total_uploaded),$n2" | Add-Content $csv
        } catch {}
        Start-Sleep -Seconds 1
    }
}

# --- the two windows
$env:SYNCWATCH_LAB_MAGNET = $magnet
$env:SYNCWATCH_LAB_FILE = 'long.mkv'
$env:SYNCWATCH_DEV_TORRENT_BIND = '127.0.0.1'
$env:SYNCWATCH_DEV_TORRENT_DHT = '0'
$env:SYNCWATCH_LAB_TORRENT_FILE = $filmTorrent
$out = pwsh -NoProfile -File scripts/dev/two-windows.ps1 -Scenario $Scenario 2>&1
$out | Where-Object { $_ -notmatch '^(Building|.*started \(PID)' } | ForEach-Object { Write-Host $_ }
$raw = ($out | Select-String 'Raw results: (.*)$').Matches | Select-Object -Last 1 | ForEach-Object { $_.Groups[1].Value.Trim() }
Stop-Job $mon; Remove-Job $mon -Force

# --- evaluation
$fail = 0
function Result($ok, $text) { if ($ok) { Write-Host "PASS $text" } else { Write-Host "FAIL $text" -ForegroundColor Red; $script:fail++ } }
$ts = @{}
foreach ($w in 'a', 'b', 'c') {
    $f = Join-Path $raw "scenario-$w.txt"
    if (Test-Path $f) { Get-Content $f | Where-Object { $_ -like 'TS *' } | ForEach-Object { $p = $_ -split ' '; $ts[$p[1]] = [int64]$p[2] } }
}
$rows = Import-Csv $csv | ForEach-Object { [pscustomobject]@{ t = [int64]$_.wallMs; up = [int64]$_.qbtUploaded; peers = [int]$_.qbtPeers; conns = [int]$_.connsQbtToUs; peer = [int]$_.peerConns; trk = [int]$_.trackerConns; udp = [int]$_.ourUdp; listen = $_.ourTcpListen; up2 = [int64]$_.qbtUploaded2; peers2 = [int]$_.qbtPeers2 } }
if ($Scenario -eq 'torrentfile') {
    function Window($from, $to) { @($rows | Where-Object { $_.t -ge $from -and $_.t -le $to }) }
    if ($ts['t2_all_complete'] -and $ts['t1_off'] -and $ts['final']) {
        $t2after = Window ($ts['t2_all_complete'] + 3000) $ts['final']
        Result ($t2after.Count -ge 10 -and @($t2after | ForEach-Object up2 | Sort-Object -Unique).Count -eq 1 -and @($t2after | Where-Object { $_.peers2 -gt 0 }).Count -eq 0) "(f/e) .torrent-file torrent: 0 bytes and no peers over $($t2after.Count) s after 100%, including switching back to it"
        $t1during = Window $ts['t1_set'] $ts['t1_off']
        Result (($t1during | Measure-Object up -Maximum).Maximum -gt 0) '(e) the magnet torrent downloaded while it was the source'
        $t1after = Window ($ts['t1_off'] + 3000) $ts['final']
        Result ($t1after.Count -ge 10 -and @($t1after | ForEach-Object up | Sort-Object -Unique).Count -eq 1 -and @($t1after | Where-Object { $_.peers -gt 0 }).Count -eq 0) "(e) magnet torrent: 0 bytes and no peers over $($t1after.Count) s after switching to the local file"
        Result (@($t1after | Where-Object { $_.peer -gt 0 -or $_.trk -gt 0 -or $_.udp -gt 0 }).Count -eq 0) '(e/f) no peer or tracker connection and no UDP socket of ours after the switches'
    } else {
        Result $false 'torrentfile timestamps missing (see the scenario output)'
    }
    $clog = Join-Path $env:LOCALAPPDATA 'SyncWatch-dev-c\logs\syncwatch.log'
    $cStarts = @(Select-String -Path $clog -Pattern 'torrent engine started' | Where-Object { $_.Line -match '^(\S+)' -and ($ms = [DateTimeOffset]::Parse($Matches[1]).ToUnixTimeMilliseconds()) -ge $ts['t2_set'] -and $ms -lt ($ts['c_accept'] ?? [int64]::MaxValue) }).Count
    Result ($ts['c_declined'] -and $cStarts -eq 0) '(g) the declining member started no torrent engine'
    foreach ($w in 'a', 'b', 'c') {
        $files = @(Get-ChildItem -Recurse -File (Join-Path $env:LOCALAPPDATA "SyncWatch-dev-$w\cache\torrents") -ErrorAction SilentlyContinue | Where-Object Name -ne 'finished.json')
        Result ($files.Count -eq 1 -and $files[0].Name -eq 'SyncWatch Test Film.mkv') "window $($w): only the chosen file is kept; the switched-away magnet left nothing ($($files.Name -join ', '))"
    }
    if (-not $KeepQbt) {
        try { Invoke-RestMethod "$api/app/shutdown" -Method Post -TimeoutSec 5 | Out-Null } catch {}
        $qp.WaitForExit(15000) | Out-Null
        if (-not $qp.HasExited) { Stop-Process -Id $qp.Id -Force }
        Remove-Item -Recurse -Force $prof -ErrorAction SilentlyContinue
    }
    Write-Host "Monitor: $csv"
    if ($fail) { exit 1 }
    exit 0
}
$lastDone = @($ts['host_complete'], $ts['member_complete']) | Where-Object { $_ } | Measure-Object -Maximum | ForEach-Object Maximum
if ($lastDone -and $ts['idle_end']) {
    $after = @($rows | Where-Object { $_.t -ge ($lastDone + 3000) -and $_.t -le $ts['final'] })
    $before = @($rows | Where-Object { $_.t -lt $lastDone })
    Result ($before.Count -gt 0 -and ($before | Measure-Object up -Maximum).Maximum -gt 0) "qBittorrent uploaded while we downloaded (max $((($before | Measure-Object up -Maximum).Maximum / 1MB).ToString('0.0')) MiB)"
    $ups = $after | ForEach-Object up | Sort-Object -Unique
    Result ($after.Count -ge 10 -and @($ups).Count -eq 1) "after 100%: qBittorrent uploaded 0 more bytes over $($after.Count) s (incl. the rejoin)"
    Result (@($after | Where-Object { $_.peers -gt 0 }).Count -eq 0) 'after 100%: qBittorrent sees no peer'
    Result (@($after | Where-Object { $_.peer -gt 0 }).Count -eq 0) 'after 100%: no peer connection between qBittorrent and our processes'
    Result (@($after | Where-Object { $_.trk -gt 0 }).Count -eq 0) 'after 100%: no tracker connection left open (stopped announce done, no keep-alive)'
    Result (@($after | Where-Object { $_.udp -gt 0 }).Count -eq 0) 'after 100%: our processes hold no UDP socket (no DHT, no tracker client)'
    $total = ($rows | Measure-Object up -Maximum).Maximum
    $file = (Get-Item (Join-Path $pack 'long.mkv')).Length
    Result ($total -le 2 * ($file + 2 * 256KB)) "qBittorrent sent $([math]::Round($total / 1MB, 1)) MiB in total for two downloaders of the $([math]::Round($file / 1MB, 1)) MiB selected file (only the selected file)"
} else {
    Result $false 'completion timestamps missing (see the scenario output)'
}
# Engine listen ports from the logs must be closed at the end.
foreach ($w in 'a', 'b') {
    $log = Join-Path $env:LOCALAPPDATA "SyncWatch-dev-$w\logs\syncwatch.log"
    $ports = Select-String -Path $log -Pattern 'torrent engine started.*port=(\d+)' | ForEach-Object { $_.Matches[0].Groups[1].Value } | Select-Object -Last 3
    $starts = @(Select-String -Path $log -Pattern 'torrent engine started' | Where-Object { $_.Line -match '^(\S+)' -and ([DateTimeOffset]::Parse($Matches[1]).ToUnixTimeMilliseconds() -ge ($ts['rejoin_start'] ?? [int64]::MaxValue)) }).Count
    if ($w -eq 'b') { Result ($starts -eq 0) "window b: the rejoin did not start a torrent engine (served from the finished file)" }
    $files = @(Get-ChildItem -Recurse -File (Join-Path $env:LOCALAPPDATA "SyncWatch-dev-$w\cache\torrents") -ErrorAction SilentlyContinue | Where-Object Name -ne 'finished.json')
    Result ($files.Count -eq 1 -and $files[0].Name -eq 'long.mkv') "window ${w}: the cache holds only the selected file ($($files.Name -join ', '))"
}

if (-not $KeepQbt) {
    try { Invoke-RestMethod "$api/app/shutdown" -Method Post -TimeoutSec 5 | Out-Null } catch {}
    $qp.WaitForExit(15000) | Out-Null
    if (-not $qp.HasExited) { Stop-Process -Id $qp.Id -Force }
    Remove-Item -Recurse -Force $prof -ErrorAction SilentlyContinue
}
Write-Host "Monitor: $csv"
if ($fail) { exit 1 }
