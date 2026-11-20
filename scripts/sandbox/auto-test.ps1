<#
.SYNOPSIS
  Automated clean-machine test, run INSIDE Windows Sandbox by scripts/sandbox-auto.wsb
  (Windows PowerShell 5.1; the sandbox has no PowerShell 7). Never run it on your own PC: it
  installs and uninstalls SyncWatch and deletes %LOCALAPPDATA%\SyncWatch.

.DESCRIPTION
  Uses the test MSIs from scripts/sandbox-prepare.ps1 (C:\SyncWatchTest, read-only):
    1. Install 0.0.1 per-user, silently.
    2. Update: SyncWatch 0.0.1 checks the local update server (signed 0.0.2), downloads,
       verifies and installs it like "Update now" + "Restart to update"; the MSI's major upgrade
       replaces 0.0.1 and starts SyncWatch again.
    3. Uninstall WITHOUT REMOVEUSERDATA: the program goes, the data and credentials stay.
    4. Reinstall 0.0.2: the kept data is used again (same identity).
    5. Uninstall WITH REMOVEUSERDATA=1: the program, the data folder and the Credential Manager
       entries go.
  Source of truth for "installed": the Windows Installer API (RelatedProducts of SyncWatch's
  fixed UpgradeCode, ProductInfo, ProductsEx context), not a guessed registry key. The
  Uninstall registry keys (HKCU, HKLM 64-bit, HKLM 32-bit) and the per-user Installer data are
  dumped before and after every step as evidence.
  Results: C:\SyncWatchResults\auto-<time>\ (result.txt, evidence.txt, msiexec /l*v logs,
  app logs, cleanup report), UTF-8.
#>
$ErrorActionPreference = 'Continue'
$src = 'C:\SyncWatchTest'
$out = Join-Path 'C:\SyncWatchResults' ("auto-" + (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Force $out | Out-Null
$result = Join-Path $out 'result.txt'
$evidence = Join-Path $out 'evidence.txt'
$script:fails = 0
$UpgradeCode = '{B04C9E7E-3415-4C46-8DCF-6B4E8DDD2472}'
$install = Join-Path $env:LOCALAPPDATA 'Programs\SyncWatch'
$data = Join-Path $env:LOCALAPPDATA 'SyncWatch'
$exe = Join-Path $install 'syncwatch.exe'
$msiexec = "$env:SystemRoot\System32\msiexec.exe"
$credPattern = '(?i)com\.syncwatch\.app'

function Log([string]$t) { Out-File -FilePath $result -Append -Encoding utf8 -InputObject $t; Write-Host $t }
function Ev([string]$t) { Out-File -FilePath $evidence -Append -Encoding utf8 -InputObject $t }
function Check([bool]$ok, [string]$what) {
    if ($ok) { Log "PASS  $what" } else { Log "FAIL  $what"; $script:fails++ }
}

$wi = New-Object -ComObject WindowsInstaller.Installer
function Get-Info([string]$code, [string]$prop) {
    try { [string]$wi.GetType().InvokeMember('ProductInfo', 'GetProperty', $null, $wi, @($code, $prop)) } catch { '' }
}
# Installed SyncWatch products (any version) by the fixed UpgradeCode.
function Get-Products {
    $list = @()
    foreach ($c in $wi.RelatedProducts($UpgradeCode)) { $list += [string]$c }
    $list
}
# The SIDs whose Installer UserData holds a SyncWatch product (a per-machine install is under S-1-5-18).
function Get-ProductSids {
    $sids = @()
    Get-ChildItem 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Installer\UserData' -ErrorAction SilentlyContinue | ForEach-Object {
        $s = $_.PSChildName
        Get-ChildItem (Join-Path $_.PSPath 'Products') -ErrorAction SilentlyContinue | ForEach-Object {
            $ip = Get-ItemProperty (Join-Path $_.PSPath 'InstallProperties') -ErrorAction SilentlyContinue
            if ($ip.DisplayName -eq 'SyncWatch') { $sids += $s }
        }
    }
    $sids
}
function Get-Creds { @((cmdkey /list) | Where-Object { $_ -match $credPattern } | ForEach-Object { $_.Trim() }) }
function Get-AppProcs { @(Get-Process syncwatch -ErrorAction SilentlyContinue) }

function Dump([string]$label) {
    Ev ''
    Ev "===== $label ($(Get-Date -Format HH:mm:ss))"
    foreach ($k in 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall', 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall', 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall') {
        $hits = @(Get-ChildItem $k -ErrorAction SilentlyContinue | ForEach-Object { $p = Get-ItemProperty $_.PSPath; if ($p.DisplayName -eq 'SyncWatch') { "  {0} DisplayVersion={1} InstallLocation={2}" -f $_.PSChildName, $p.DisplayVersion, $p.InstallLocation } })
        Ev "$k : $($hits.Count) SyncWatch entr$(if ($hits.Count -eq 1) { 'y' } else { 'ies' })"
        $hits | ForEach-Object { Ev $_ }
    }
    Get-ChildItem 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Installer\UserData' -ErrorAction SilentlyContinue | ForEach-Object {
        $sid = $_.PSChildName
        Get-ChildItem (Join-Path $_.PSPath 'Products') -ErrorAction SilentlyContinue | ForEach-Object {
            $ip = Get-ItemProperty (Join-Path $_.PSPath 'InstallProperties') -ErrorAction SilentlyContinue
            if ($ip.DisplayName -eq 'SyncWatch') { Ev ("Installer UserData sid={0} DisplayVersion={1}" -f $sid, $ip.DisplayVersion) }
        }
    }
    foreach ($c in Get-Products) { Ev ("MSI API: {0} version={1} AssignmentType={2} location={3}" -f $c, (Get-Info $c 'VersionString'), (Get-Info $c 'AssignmentType'), (Get-Info $c 'InstallLocation')) }
    Ev "install folder: $(if (Test-Path $install) { (Get-ChildItem $install | ForEach-Object Name) -join ', ' } else { 'absent' })"
    Ev "data folder: $(if (Test-Path $data) { 'present' } else { 'absent' })"
    Ev "credentials: $((Get-Creds) -join '; ')"
    Get-CimInstance Win32_Process -Filter "Name='syncwatch.exe' OR Name='msiexec.exe'" -ErrorAction SilentlyContinue | ForEach-Object {
        $o = Invoke-CimMethod -InputObject $_ -MethodName GetOwner
        Ev ("process {0} pid={1} start={2} owner={3}\{4} parent={5}" -f $_.Name, $_.ProcessId, $_.CreationDate, $o.Domain, $o.User, $_.ParentProcessId)
    }
}
function Run-Msi([string[]]$msiArgs, [string]$logName) {
    $p = Start-Process $msiexec -ArgumentList ($msiArgs + @('/l*v', "`"$out\$logName`"")) -Wait -PassThru
    $p.ExitCode
}
function Stop-App {
    Get-AppProcs | Stop-Process -Force -ErrorAction SilentlyContinue
    $end = (Get-Date).AddSeconds(15)
    while ((Get-AppProcs).Count -and (Get-Date) -lt $end) { Start-Sleep -Milliseconds 300 }
}
function Remove-CleanupReports {
    foreach ($f in "$env:TEMP\SyncWatch-cleanup.log", "$env:SystemRoot\Temp\SyncWatch-cleanup.log") { Remove-Item $f -Force -ErrorAction SilentlyContinue }
}

Log "SyncWatch clean-machine auto test, $(Get-Date -Format s)"
Log "Windows $([Environment]::OSVersion.Version), user $env:USERDOMAIN\$env:USERNAME, SID $([Security.Principal.WindowsIdentity]::GetCurrent().User.Value)"
$wv = Get-ItemProperty 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}' -ErrorAction SilentlyContinue
Log "INFO  WebView2 runtime before install: $(if ($wv) { $wv.pv } else { 'not found' })"
Log "INFO  system vulkan-1.dll: $(Test-Path "$env:SystemRoot\System32\vulkan-1.dll")"
Dump 'before install'

# 1. install 0.0.1 ---------------------------------------------------------------------------
Log ''
Log '--- 1. install 0.0.1'
$code = Run-Msi @('/i', "`"$src\SyncWatch-0.0.1-x64.msi`"", '/qn') 'install-0.0.1.log'
Check ($code -eq 0) "install 0.0.1 silently (exit $code)"
Dump 'after install 0.0.1'
foreach ($f in 'syncwatch.exe', 'libmpv-2.dll', 'vulkan\vulkan-1.dll', 'default-config.toml', 'licenses\SyncWatch-GPL-3.0.txt') {
    Check (Test-Path (Join-Path $install $f)) "installed file $f"
}
$prods = @(Get-Products)
Check ($prods.Count -eq 1) "Windows Installer knows exactly one SyncWatch product ($($prods.Count))"
$p1 = if ($prods.Count) { $prods[0] } else { '' }
Check ((Get-Info $p1 'VersionString') -eq '0.0.1') "its version is 0.0.1 ($(Get-Info $p1 'VersionString'))"
# AssignmentType: 0 = per-user, 1 = per-machine (Windows Installer ProductInfo). The first run
# read the context through ProductsEx, which does not enumerate in Windows PowerShell 5.1 (it
# always gave 0); AssignmentType and the UserData SID are read directly.
Check ((Get-Info $p1 'AssignmentType') -eq '0') "installed per-user (AssignmentType $(Get-Info $p1 'AssignmentType'); 0 = per-user)"
$mySid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$psids = @(Get-ProductSids)
Check ($psids.Count -eq 1 -and $psids[0] -eq $mySid) "the product is registered for this user's SID only ($($psids -join ', '))"
Check ((Get-Info $p1 'InstallLocation').TrimEnd('\') -eq $install) 'install location is %LOCALAPPDATA%\Programs\SyncWatch'
Check (-not (Test-Path "$env:ProgramFiles\SyncWatch") -and -not (Test-Path "${env:ProgramFiles(x86)}\SyncWatch")) 'nothing in Program Files'

# 2. update to 0.0.2 through the app ----------------------------------------------------------------
Log ''
Log '--- 2. update 0.0.1 -> 0.0.2 through the app'
$server = Start-Process powershell.exe -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$src\serve-update.ps1`"", '-Dir', "`"$src\update`"", '-Port', '8765', '-Minutes', '20' -PassThru -RedirectStandardOutput "$out\server.log"
Start-Sleep -Seconds 2
$env:SYNCWATCH_DEV_UPDATE_URL = 'http://127.0.0.1:8765/'
$env:SYNCWATCH_DEV_UPDATE_PUBKEY = (Get-Content (Join-Path $src 'test-updater.key.pub') -Raw).Trim()
$env:SYNCWATCH_TEST_AUTO_UPDATE = '1'
$env:SYNCWATCH_TEST_MSI_LOG = "$out\upgrade-0.0.2.log"
$t0 = Get-Date
$old = Start-Process $exe -PassThru
Remove-Item Env:SYNCWATCH_TEST_AUTO_UPDATE, Env:SYNCWATCH_TEST_MSI_LOG, Env:SYNCWATCH_DEV_UPDATE_URL, Env:SYNCWATCH_DEV_UPDATE_PUBKEY
Check ($null -ne $old) "SyncWatch 0.0.1 starts (pid $($old.Id))"
# a) the old app quits to install the update
$deadline = (Get-Date).AddMinutes(3)
while (-not $old.HasExited -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500 }
$tQuit = Get-Date
Check $old.HasExited ("0.0.1 quit to install the update after {0:N0} s" -f ($tQuit - $t0).TotalSeconds)
# b) Windows Installer reports 0.0.2 only; c) a new SyncWatch process starts
$deadline = (Get-Date).AddMinutes(10)
$tMsi = $null; $new = $null
while ((Get-Date) -lt $deadline) {
    $ps = @(Get-Products)
    if (-not $tMsi -and $ps.Count -eq 1 -and (Get-Info $ps[0] 'VersionString') -eq '0.0.2') { $tMsi = Get-Date }
    $new = Get-AppProcs | Where-Object { $_.Id -ne $old.Id -and $_.Path -eq $exe } | Select-Object -First 1
    if ($tMsi -and $new) { break }
    Start-Sleep -Milliseconds 500
}
$tNew = Get-Date
Dump 'after the update'
Check ($null -ne $tMsi) ("Windows Installer reports 0.0.2 {0:N0} s after 0.0.1 quit" -f $(if ($tMsi) { ($tMsi - $tQuit).TotalSeconds } else { -1 }))
$prods = @(Get-Products)
Check ($prods.Count -eq 1) "exactly one SyncWatch product after the major upgrade (0.0.1 replaced; $($prods.Count) found)"
Check ($null -ne $new) ("SyncWatch started again after the update (relaunch {0:N0} s after 0.0.1 quit)" -f ($tNew - $tQuit).TotalSeconds)
if ($new) {
    $w = Get-CimInstance Win32_Process -Filter "ProcessId=$($new.Id)"
    $par = Get-CimInstance Win32_Process -Filter "ProcessId=$($w.ParentProcessId)" -ErrorAction SilentlyContinue
    $own = Invoke-CimMethod -InputObject $w -MethodName GetOwner
    Log ("INFO  relaunched pid {0}, owner {1}\{2}, parent {3} ({4})" -f $new.Id, $own.Domain, $own.User, $(if ($par) { $par.Name } else { 'exited' }), $w.ParentProcessId)
    Check ($own.User -eq $env:USERNAME) 'the relaunched app runs as the signed-in user'
}
$fv = (Get-Item $exe -ErrorAction SilentlyContinue).VersionInfo.ProductVersion
Check ("$fv" -like '0.0.2*') "the installed syncwatch.exe is 0.0.2 ($fv)"
$served = Get-Content "$out\server.log" -ErrorAction SilentlyContinue
Check (@($served | Where-Object { $_ -match '200 GET /SyncWatch-0.0.2-x64.msi' }).Count -ge 1) 'the update was downloaded from the local server'
$up = Get-Content "$out\upgrade-0.0.2.log" -Encoding Unicode -ErrorAction SilentlyContinue
Check (@($up | Where-Object { $_ -match 'Action start .*LaunchApplication' }).Count -ge 1) 'the upgrade log shows the MSI starting SyncWatch (LaunchApplication)'
Check (@($up | Where-Object { $_ -match 'Product Version: 0\.0\.2.*(success|status): 0' }).Count -ge 1) 'the upgrade log shows 0.0.2 installed with status 0'
Check (Test-Path $data) 'the data folder exists'
$credsBefore = Get-Creds
Check ($credsBefore.Count -ge 1) "SyncWatch has a Credential Manager entry (identity): $($credsBefore -join '; ')"

# 3. uninstall WITHOUT REMOVEUSERDATA: data and credentials stay -------------------------------------
Log ''
Log '--- 3. uninstall without REMOVEUSERDATA'
Start-Sleep -Seconds 5
Stop-App
Copy-Item (Join-Path $data 'logs') (Join-Path $out 'app-logs-before-keep') -Recurse -ErrorAction SilentlyContinue
Remove-CleanupReports
$p2 = if ($prods.Count) { $prods[0] } else { '' }
if (-not $p2) {
    Check $false 'a product code to uninstall (none found: skipping)'
} else {
    $code = Run-Msi @('/x', $p2, '/qn') 'uninstall-keep-data.log'
    Check ($code -eq 0) "uninstall $p2 (exit $code)"
}
Start-Sleep -Seconds 2
Dump 'after uninstall without REMOVEUSERDATA'
Check (@(Get-Products).Count -eq 0) 'Windows Installer: SyncWatch is no longer installed'
Check (-not (Test-Path $exe)) 'the program files are removed'
Check (Test-Path $data) 'the data folder is KEPT (REMOVEUSERDATA not set)'
Check ((Get-Creds).Count -eq $credsBefore.Count) 'the Credential Manager entries are KEPT'
Check (-not (Test-Path "$env:TEMP\SyncWatch-cleanup.log") -and -not (Test-Path "$env:SystemRoot\Temp\SyncWatch-cleanup.log")) 'the cleanup did not run'

# 4. reinstall 0.0.2: the kept data is used again -------------------------------------------------------
Log ''
Log '--- 4. reinstall 0.0.2'
$code = Run-Msi @('/i', "`"$src\update\SyncWatch-0.0.2-x64.msi`"", '/qn') 'install-0.0.2.log'
Check ($code -eq 0) "install 0.0.2 silently (exit $code)"
# The sandbox has a system vulkan-1.dll, so force the bundled loader to prove it loads and works.
$env:SYNCWATCH_FORCE_BUNDLED_VULKAN = '1'
$app = Start-Process $exe -PassThru
Remove-Item Env:SYNCWATCH_FORCE_BUNDLED_VULKAN
Start-Sleep -Seconds 12
Check (-not $app.HasExited) 'SyncWatch 0.0.2 runs with the bundled Vulkan loader forced (12 s)'
$log = Get-ChildItem (Join-Path $data 'logs') -File -ErrorAction SilentlyContinue | Sort-Object LastWriteTime | Select-Object -Last 1
$lines = if ($log) { Get-Content $log.FullName } else { @() }
$starts = @($lines | Select-String 'app version').Count
$created = @($lines | Select-String 'created a new identity').Count
Check (@($lines | Select-String 'native libraries loaded vulkan=Bundled').Count -ge 1) 'the log shows the bundled Vulkan loader in use'
Check ($created -eq 1 -and $starts -ge 3) "the kept identity is used again (new identities created: $created over $starts starts)"
Stop-App
Copy-Item (Join-Path $data 'logs') (Join-Path $out 'app-logs') -Recurse -ErrorAction SilentlyContinue
$credsBefore = Get-Creds
Dump 'after reinstall'

# 5. uninstall WITH REMOVEUSERDATA=1 ---------------------------------------------------------------------
Log ''
Log '--- 5. uninstall with REMOVEUSERDATA=1'
Remove-CleanupReports
$p3 = @(Get-Products) | Select-Object -First 1
if (-not $p3) {
    Check $false 'a product code to uninstall (none found: skipping)'
} else {
    $code = Run-Msi @('/x', $p3, '/qn', 'REMOVEUSERDATA=1') 'uninstall-remove-data.log'
    Check ($code -eq 0) "uninstall $p3 with REMOVEUSERDATA=1 (exit $code)"
}
Start-Sleep -Seconds 2
Dump 'after uninstall with REMOVEUSERDATA=1'
Check (@(Get-Products).Count -eq 0) 'Windows Installer: SyncWatch is no longer installed'
Check (-not (Test-Path $install) -or @(Get-ChildItem $install -Recurse -Force).Count -eq 0) 'the program folder is removed'
Check (-not (Test-Path $data)) 'the data folder %LOCALAPPDATA%\SyncWatch is removed'
Check ((Get-Creds).Count -eq 0) "no SyncWatch entries left in Credential Manager (before: $($credsBefore.Count))"
$ul = Get-Content "$out\uninstall-remove-data.log" -Encoding Unicode -ErrorAction SilentlyContinue
$ca = @($ul | Where-Object { $_ -match 'CustomActionSchedule\(Action=CleanupUserData' })
Check ($ca.Count -ge 1) 'the uninstall log shows the CleanupUserData action'
Log "INFO  $($ca | Select-Object -First 1)"
$rep = @("$env:TEMP\SyncWatch-cleanup.log", "$env:SystemRoot\Temp\SyncWatch-cleanup.log") | Where-Object { Test-Path $_ } | Select-Object -First 1
Check ($null -ne $rep) "the cleanup wrote its report ($rep)"
if ($rep) {
    Copy-Item $rep (Join-Path $out 'cleanup-report.txt')
    $r = Get-Content $rep
    $r | ForEach-Object { Log "INFO  cleanup: $_" }
    Check (@($r | Where-Object { $_ -match "^user=.*\\$([regex]::Escape($env:USERNAME))$" }).Count -eq 1) "the cleanup ran as the signed-in user $env:USERNAME"
}
Log "INFO  left in Programs: $(if (Test-Path $install) { (Get-ChildItem $install -Recurse -Force | ForEach-Object FullName) -join ', ' } else { 'nothing' })"
Log "INFO  SyncWatch processes left: $((Get-AppProcs).Count)"

if ($server -and -not $server.HasExited) { Stop-Process -Id $server.Id -Force }
Log ''
if ($script:fails -eq 0) { Log 'RESULT: ALL PASSED' } else { Log "RESULT: $($script:fails) FAILED" }
Start-Process notepad.exe $result
