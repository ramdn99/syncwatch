#Requires -Version 7.0
<#
.SYNOPSIS
  Idle CPU and memory of SyncWatch (Milestone 9): two windows in one room, optimized build
  (Cargo profile "measure": release optimizations plus the dev automation). No mouse or
  keyboard input, no relay quota (-Ice none), nothing left running.

.DESCRIPTION
  -Part room     two windows join one room (two-windows.ps1 -Soak), nothing loaded, window b
                 minimized; samples over the middle of the soak.
  -Part torrent  the torrent lab (local qBittorrent seeder): both windows finish the torrent,
                 then the host holds an idle phase of -Seconds with the finished file playing
                 in the room; samples only inside that phase (TS idle_start .. idle_end).
  Each SyncWatch process is measured together with its WebView2 child processes (the UI and
  the bridge). CPU is reported as percent of ONE logical core; memory as private bytes and
  working set. Results: printed, and appended as CSV to %TEMP%\syncwatch-idle-measure.csv.

  The UI comes from the Vite dev server in these runs (React development mode), so the numbers
  are an upper bound for the shipped build.

.EXAMPLE
  pwsh -NoProfile -File scripts/dev/idle-measure.ps1 -Part room -Seconds 120
.EXAMPLE
  pwsh -NoProfile -File scripts/dev/idle-measure.ps1 -Part torrent -Seconds 120
#>
param(
    [ValidateSet('room', 'torrent')][string]$Part = 'room',
    [int]$Seconds = 120
)
$ErrorActionPreference = 'Stop'
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
Set-Location $root
$samples = Join-Path ([IO.Path]::GetTempPath()) "sw-idle-samples-$PID.csv"
$stopFile = "$samples.stop"

# Sampler: every second, each syncwatch.exe with all its descendants.
$sampler = Start-ThreadJob -ArgumentList $samples, $stopFile -ScriptBlock {
    param($csv, $stop)
    'ms,root,cpuSec,privMB,wsMB,procs' | Set-Content $csv
    while (-not (Test-Path $stop)) {
        $all = @(Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId, Name)
        $kids = @{}
        foreach ($p in $all) { $kids[[int]$p.ParentProcessId] += @([int]$p.ProcessId) }
        $now = [DateTimeOffset]::Now.ToUnixTimeMilliseconds()
        foreach ($r in @($all | Where-Object Name -EQ 'syncwatch.exe')) {
            $ids = [Collections.Generic.List[int]]::new(); $ids.Add([int]$r.ProcessId)
            for ($i = 0; $i -lt $ids.Count; $i++) { foreach ($k in @($kids[$ids[$i]])) { if ($k -and -not $ids.Contains($k)) { $ids.Add($k) } } }
            $cpu = 0.0; $priv = 0L; $ws = 0L; $n = 0
            foreach ($id in $ids) {
                $gp = Get-Process -Id $id -ErrorAction SilentlyContinue
                if (-not $gp) { continue }
                $cpu += $gp.TotalProcessorTime.TotalSeconds; $priv += $gp.PrivateMemorySize64; $ws += $gp.WorkingSet64; $n++
            }
            '{0},{1},{2:F3},{3:F1},{4:F1},{5}' -f $now, $r.ProcessId, $cpu, ($priv / 1MB), ($ws / 1MB), $n | Add-Content $csv
        }
        Start-Sleep -Seconds 1
    }
}

$t0 = [DateTimeOffset]::Now.ToUnixTimeMilliseconds()
try {
    if ($Part -eq 'room') {
        pwsh -NoProfile -File scripts/dev/two-windows.ps1 -Soak $Seconds -BuildProfile measure | Out-Host
        $t1 = [DateTimeOffset]::Now.ToUnixTimeMilliseconds()
        # Skip the start (build, join) and the end: the middle of the soak.
        $from = $t1 - ($Seconds * 1000) + 15000; $to = $t1 - 20000
    } else {
        $env:SYNCWATCH_DEV_IDLE_SECS = "$Seconds"
        pwsh -NoProfile -File scripts/dev/torrent-lab.ps1 -BuildProfile measure | Out-Host
        Remove-Item Env:SYNCWATCH_DEV_IDLE_SECS
        $fa = Get-ChildItem ([IO.Path]::GetTempPath()) -Directory -Filter 'syncwatch-two-windows-*' |
            Sort-Object LastWriteTime -Descending | Select-Object -First 1 | ForEach-Object { Join-Path $_.FullName 'scenario-a.txt' }
        $ts = @{}
        foreach ($l in (Get-Content $fa)) { if ($l -match '^TS (idle_start|idle_end) (\d+)') { $ts[$Matches[1]] = [long]$Matches[2] } }
        if (-not $ts.idle_start -or -not $ts.idle_end) { throw 'the torrent run did not reach its idle phase' }
        $from = $ts.idle_start + 5000; $to = $ts.idle_end
    }
} finally {
    New-Item -ItemType File $stopFile -Force | Out-Null
    Receive-Job $sampler -Wait -AutoRemoveJob | Out-Null
    Remove-Item $stopFile -ErrorAction SilentlyContinue
}

$rows = Import-Csv $samples | Where-Object { [long]$_.ms -ge $from -and [long]$_.ms -le $to }
$cores = [Environment]::ProcessorCount
$out = Join-Path ([IO.Path]::GetTempPath()) 'syncwatch-idle-measure.csv'
if (-not (Test-Path $out)) { 'date,part,window,seconds,cpuPctOneCore,privMBmedian,wsMBmedian,procs' | Set-Content $out }
$i = 0
foreach ($g in ($rows | Group-Object root)) {
    $s = @($g.Group | Sort-Object { [long]$_.ms })
    if ($s.Count -lt 10) { continue }
    $i++
    $dt = ([long]$s[-1].ms - [long]$s[0].ms) / 1000
    $cpu = ([double]$s[-1].cpuSec - [double]$s[0].cpuSec) / $dt * 100
    $med = { param($k) ($s | ForEach-Object { [double]$_.$k } | Sort-Object)[[int]($s.Count / 2)] }
    $priv = & $med 'privMB'; $ws = & $med 'wsMB'
    $label = if ($Part -eq 'room') { @('host a', 'member b (minimized)')[$i - 1] } else { @('host a', 'member b')[$i - 1] }
    '{0,-22} {1,5:N0} s   CPU {2,5:N2}% of one core ({3:N2}% of {4} cores)   private {5,6:N0} MB   working set {6,6:N0} MB   ({7} processes)' -f $label, $dt, $cpu, ($cpu / $cores), $cores, $priv, $ws, $s[-1].procs
    '{0},{1},{2},{3:N0},{4:N2},{5:N0},{6:N0},{7}' -f (Get-Date -Format s), $Part, $label, $dt, $cpu, $priv, $ws, $s[-1].procs | Add-Content $out
}
Remove-Item $samples -ErrorAction SilentlyContinue
if ($i -eq 0) { Write-Host 'No samples in the measuring window.' -ForegroundColor Red; exit 1 }
Write-Host "Appended to $out"
