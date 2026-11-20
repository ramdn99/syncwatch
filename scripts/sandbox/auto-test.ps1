<#
.SYNOPSIS
  Automated clean-machine test, run INSIDE Windows Sandbox by scripts/sandbox-auto.wsb
  (Windows PowerShell 5.1; the sandbox has no PowerShell 7). Never run it on your own PC: it
  installs and uninstalls SyncWatch and deletes %LOCALAPPDATA%\SyncWatch.

.DESCRIPTION
  Uses the test MSIs from scripts/sandbox-prepare.ps1 (C:\SyncWatchTest, read-only):
    1. Installs SyncWatch 0.0.1 per-user, silently; checks the files, the per-user registration
       and that nothing needed administrator rights.
    2. Starts the local update server (serve-update.ps1) with the signed 0.0.2 release.
    3. Starts SyncWatch 0.0.1 with the test update URL and test key (test MSIs only accept
       these). Its test hook checks, downloads, verifies and installs the update like "Update now"
       and "Restart to update"; the MSI then starts the new version.
    4. Waits for 0.0.2: registered version, file version, a running process.
    5. Uninstalls with REMOVEUSERDATA=1 and checks that the program, the data folder and the
       Credential Manager entries are gone.
  Writes C:\SyncWatchResults\auto-<time>\result.txt (PASS/FAIL lines) with the MSI and app logs,
  then opens it.
#>
$ErrorActionPreference = 'Continue'
$src = 'C:\SyncWatchTest'
$outRoot = 'C:\SyncWatchResults'
$out = Join-Path $outRoot ("auto-" + (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Force $out | Out-Null
$result = Join-Path $out 'result.txt'
$script:fails = 0
function Log([string]$t) { $t | Tee-Object -FilePath $result -Append | Out-Host }
function Check([bool]$ok, [string]$what) {
    if ($ok) { Log "PASS  $what" } else { Log "FAIL  $what"; $script:fails++ }
}
function Get-Entry {
    Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall' -ErrorAction SilentlyContinue |
        ForEach-Object { Get-ItemProperty $_.PSPath } | Where-Object { $_.DisplayName -eq 'SyncWatch' } | Select-Object -First 1
}
$install = Join-Path $env:LOCALAPPDATA 'Programs\SyncWatch'
$data = Join-Path $env:LOCALAPPDATA 'SyncWatch'
$exe = Join-Path $install 'syncwatch.exe'

Log "SyncWatch clean-machine auto test, $(Get-Date -Format s)"
Log "Windows $([Environment]::OSVersion.Version), user $env:USERNAME"
$wv = Get-ItemProperty 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}' -ErrorAction SilentlyContinue
Log "WebView2 runtime before install: $(if ($wv) { $wv.pv } else { 'not found' })"

# 1. install 0.0.1 ---------------------------------------------------------------------------
$msi1 = Join-Path $src 'SyncWatch-0.0.1-x64.msi'
$p = Start-Process "$env:SystemRoot\System32\msiexec.exe" -ArgumentList '/i', "`"$msi1`"", '/qn', '/l*v', "`"$out\install-0.0.1.log`"" -Wait -PassThru
Check ($p.ExitCode -eq 0) "install 0.0.1 silently (exit $($p.ExitCode))"
foreach ($f in 'syncwatch.exe', 'libmpv-2.dll', 'vulkan\vulkan-1.dll', 'default-config.toml', 'licenses\SyncWatch-GPL-3.0.txt') {
    Check (Test-Path (Join-Path $install $f)) "installed file $f"
}
$e = Get-Entry
Check ($e -and $e.DisplayVersion -eq '0.0.1') "registered per-user (HKCU) as version $($e.DisplayVersion)"
Check (-not (Test-Path "$env:ProgramFiles\SyncWatch")) 'nothing in Program Files'
$wv2 = Get-ItemProperty 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}' -ErrorAction SilentlyContinue
Log "WebView2 runtime after install: $(if ($wv2) { $wv2.pv } else { 'not found' })"

# 2. local update server -------------------------------------------------------------------------
$server = Start-Process powershell.exe -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$src\serve-update.ps1`"", '-Dir', "`"$src\update`"", '-Port', '8765', '-Minutes', '15' -PassThru -RedirectStandardOutput "$out\server.log"
Start-Sleep -Seconds 2

# 3. start 0.0.1 with the test update source ------------------------------------------------------
$env:SYNCWATCH_DEV_UPDATE_URL = 'http://127.0.0.1:8765/'
$env:SYNCWATCH_DEV_UPDATE_PUBKEY = (Get-Content (Join-Path $src 'test-updater.key.pub') -Raw).Trim()
$env:SYNCWATCH_TEST_AUTO_UPDATE = '1'
$t0 = Get-Date
$app = Start-Process $exe -PassThru
Check ($null -ne $app) 'SyncWatch 0.0.1 starts'
Start-Sleep -Seconds 8
Check (-not $app.HasExited -or (Get-Entry).DisplayVersion -eq '0.0.2') 'SyncWatch 0.0.1 is still running after 8 s (or already updating)'

# 4. wait for 0.0.2 ----------------------------------------------------------------------------
$deadline = (Get-Date).AddMinutes(6)
$updated = $false
while ((Get-Date) -lt $deadline) {
    $e = Get-Entry
    $running = @(Get-Process syncwatch -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe -and $_.StartTime -gt $t0.AddSeconds(5) })
    if ($e -and $e.DisplayVersion -eq '0.0.2' -and $running.Count -gt 0) { $updated = $true; break }
    Start-Sleep -Seconds 3
}
Check $updated ("updated to 0.0.2 and the new version started again ({0:N0} s)" -f ((Get-Date) - $t0).TotalSeconds)
$fv = (Get-Item $exe -ErrorAction SilentlyContinue).VersionInfo.ProductVersion
Log "INFO  syncwatch.exe product version now: $fv"
Check ("$fv" -like '0.0.2*') 'the installed program is 0.0.2'
Check (@(Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall' | ForEach-Object { Get-ItemProperty $_.PSPath } | Where-Object DisplayName -EQ 'SyncWatch').Count -eq 1) 'exactly one SyncWatch entry (the old one was replaced)'
$served = Get-Content "$out\server.log" -ErrorAction SilentlyContinue
Check (@($served | Where-Object { $_ -match '200 GET /SyncWatch-0.0.2-x64.msi' }).Count -ge 1) 'the update was downloaded from the local server'
Check (Test-Path $data) 'the data folder exists while installed'

# 5. uninstall with data removal -------------------------------------------------------------------
Get-Process syncwatch -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 2
Copy-Item (Join-Path $data 'logs') (Join-Path $out 'app-logs') -Recurse -ErrorAction SilentlyContinue
$e = Get-Entry
$code = if ($e) { $e.PSChildName } else { '' }
$p = Start-Process "$env:SystemRoot\System32\msiexec.exe" -ArgumentList '/x', $code, '/qn', 'REMOVEUSERDATA=1', '/l*v', "`"$out\uninstall.log`"" -Wait -PassThru
Check ($p.ExitCode -eq 0) "uninstall with REMOVEUSERDATA=1 (exit $($p.ExitCode))"
Start-Sleep -Seconds 3
Check (-not (Test-Path $exe)) 'the program is removed'
Check (-not (Get-Entry)) 'the uninstall entry is removed'
Check (-not (Test-Path $data)) 'the data folder %LOCALAPPDATA%\SyncWatch is removed'
$creds = (cmdkey /list) -join "`n"
Check ($creds -notmatch '(?i)syncwatch|metered-credentials|identity-ed25519') 'no SyncWatch entries left in Credential Manager'

if ($server -and -not $server.HasExited) { Stop-Process -Id $server.Id -Force }
Log ''
if ($script:fails -eq 0) { Log 'RESULT: ALL PASSED' } else { Log "RESULT: $($script:fails) FAILED" }
Start-Process notepad.exe $result
