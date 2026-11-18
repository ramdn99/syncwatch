#Requires -Version 7.0
<#
.SYNOPSIS
  Portable mode check (Milestone 8): with portable.flag next to the exe, everything goes to
  <exe>\data and nothing is written anywhere else (install folder, %LOCALAPPDATA%, %APPDATA%).

.DESCRIPTION
  Copies the built app (target\debug or -Release) and libmpv into a fresh temporary "install"
  folder with portable.flag, snapshots the SyncWatch/Tauri folders under %LOCALAPPDATA% and
  %APPDATA%, starts the app for -Seconds, closes it, and checks:
    - data\config, data\logs, data\cache and data\webview2 exist and the log was written there;
    - the install folder gained nothing but data\ (and portable.flag);
    - no SyncWatch or com.syncwatch.app folder appeared or changed under %LOCALAPPDATA% or
      %APPDATA%.
  No mouse or keyboard input. Cleans up the temporary folder.

.EXAMPLE
  pwsh -NoProfile -File scripts/dev/portable-check.ps1
#>
param([int]$Seconds = 15, [switch]$Release)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$bin = Join-Path $repo ($(if ($Release) { 'target\release' } else { 'target\debug' }))
$exe = Join-Path $bin 'syncwatch.exe'
if (-not (Test-Path $exe)) { Write-Host "Build first: cargo build -p syncwatch-desktop" -ForegroundColor Red; exit 1 }

$root = Join-Path ([IO.Path]::GetTempPath()) "sw-portable-$PID"
$inst = Join-Path $root 'SyncWatch'
New-Item -ItemType Directory -Force $inst | Out-Null
Copy-Item $exe $inst
foreach ($n in 'libmpv-2.dll') { if (Test-Path (Join-Path $bin $n)) { Copy-Item (Join-Path $bin $n) $inst } }
foreach ($d in 'fonts', 'vulkan') { if (Test-Path (Join-Path $bin $d)) { Copy-Item -Recurse (Join-Path $bin $d) $inst } }
New-Item -ItemType File (Join-Path $inst 'portable.flag') | Out-Null
$before = @(Get-ChildItem -Recurse -File $inst | ForEach-Object { $_.FullName })

function Snapshot {
    $roots = @($env:LOCALAPPDATA, $env:APPDATA)
    $roots | ForEach-Object { Get-ChildItem $_ -Directory -ErrorAction SilentlyContinue } |
        Where-Object { $_.Name -match '^(SyncWatch|com\.syncwatch)' } |
        ForEach-Object {
            $last = Get-ChildItem -Recurse -File $_.FullName -ErrorAction SilentlyContinue | Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1
            [pscustomobject]@{ Path = $_.FullName; Last = $last.LastWriteTimeUtc }
        }
}
$snap = @(Snapshot)
$t0 = (Get-Date).ToUniversalTime()

# A plain start: no dev identity, no dev scenario.
$envKeep = @{}
foreach ($k in 'SYNCWATCH_DEV_IDENTITY', 'SYNCWATCH_DEV_AUTOROOM', 'SYNCWATCH_DEV_SCENARIO_FILE', 'WEBVIEW2_USER_DATA_FOLDER') {
    $envKeep[$k] = [Environment]::GetEnvironmentVariable($k)
    Remove-Item "Env:$k" -ErrorAction SilentlyContinue
}
# PowerShell turns $null into an empty value here: the app must treat an empty
# WEBVIEW2_USER_DATA_FOLDER as unset (otherwise WebView2 writes next to the exe).
[Environment]::SetEnvironmentVariable('WEBVIEW2_USER_DATA_FOLDER', $null)
$p = Start-Process (Join-Path $inst 'syncwatch.exe') -WorkingDirectory $inst -PassThru
Start-Sleep -Seconds $Seconds
if (-not $p.HasExited) { [void]$p.CloseMainWindow(); Start-Sleep -Seconds 4 }
if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force }
Get-Process msedgewebview2 -ErrorAction SilentlyContinue | Where-Object { $_.Path -and (Get-CimInstance Win32_Process -Filter "ProcessId=$($_.Id)").CommandLine -match [regex]::Escape($inst) } | Stop-Process -Force -ErrorAction SilentlyContinue
foreach ($k in $envKeep.Keys) { if ($null -ne $envKeep[$k]) { Set-Item "Env:$k" $envKeep[$k] } }
Start-Sleep -Seconds 2

$fail = 0
function Result($ok, $text) { if ($ok) { Write-Host "PASS  $text" } else { Write-Host "FAIL  $text" -ForegroundColor Red; $script:fail++ } }
$data = Join-Path $inst 'data'
foreach ($d in 'config', 'logs', 'cache', 'webview2') { Result (Test-Path (Join-Path $data $d)) "data\$d exists" }
$log = Join-Path $data 'logs\syncwatch.log'
Result ((Test-Path $log) -and ((Get-Content $log -Raw) -match 'portable=true')) 'the log is in data\logs and says portable=true'
$after = @(Get-ChildItem -Recurse -File $inst | ForEach-Object { $_.FullName })
$outside = @($after | Where-Object { $_ -notin $before -and -not $_.StartsWith($data + '\') })
Result ($outside.Count -eq 0) "nothing written in the install folder outside data\ ($($outside -join ', '))"
$now = @(Snapshot)
$changed = @($now | Where-Object {
        $n = $_
        $b = $snap | Where-Object Path -EQ $n.Path
        -not $b -or ($n.Last -and $n.Last -gt $t0)
    })
Result ($changed.Count -eq 0) "no SyncWatch or com.syncwatch.app folder created or changed in %LOCALAPPDATA% or %APPDATA% ($($changed.Path -join ', '))"
if ($fail -and (Test-Path $log)) { Get-Content $log -TotalCount 4 | ForEach-Object { "  log: $_" } }
Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
if ($fail) { exit 1 }
