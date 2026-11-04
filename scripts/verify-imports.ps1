#Requires -Version 7.0
<#
.SYNOPSIS
  Verifies that shipped binaries import only Windows system DLLs (spec section 3).

.DESCRIPTION
  Uses dumpbin /dependents (found under the VS Build Tools; no dev shell needed) and checks
  both normal and delay-load imports against an allowlist:
    - Windows system DLLs listed below, plus api-ms-win-* / ext-ms-win-* API sets
      (api-ms-win-crt-* and msvcrt.dll count as system: libmpv is MinGW-built).
    - App DLLs shipped next to the exe: libmpv-2.dll (only as an import of syncwatch.exe).
    - vulkan-1.dll is the ONLY non-system exception, and only for libmpv-2.dll.
  The VC++ runtime (vcruntime*, msvcp*, concrt*) is always forbidden: we link the CRT statically.

.PARAMETER Path
  Binaries to check. Default: target\<Profile>\syncwatch.exe and libmpv-2.dll next to it.

.PARAMETER NoVulkanException
  Test switch: drop the vulkan-1.dll exception (used by the negative test).

.PARAMETER Deny
  Test switch: extra DLL names to treat as forbidden even if allowlisted.

.EXAMPLE
  pwsh -NoProfile -File scripts/verify-imports.ps1
.EXAMPLE
  pwsh -NoProfile -File scripts/verify-imports.ps1 -Profile release
#>
[CmdletBinding()]
param(
    [string[]]$Path,
    [ValidateSet('debug', 'release')]
    [string]$Profile = 'debug',
    [switch]$NoVulkanException,
    [string[]]$Deny = @()
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path $PSScriptRoot -Parent

if (-not $Path) {
    $dir = Join-Path $root "target/$Profile"
    $Path = @((Join-Path $dir 'syncwatch.exe'), (Join-Path $dir 'libmpv-2.dll'))
}

function Find-Dumpbin {
    $cmd = Get-Command dumpbin -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $roots = @('D:\VS\BuildTools')
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $vswhere) {
        $roots += @(& $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null)
    }
    foreach ($r in $roots | Where-Object { $_ }) {
        $d = Get-ChildItem (Join-Path $r 'VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe') -ErrorAction SilentlyContinue |
            Sort-Object FullName -Descending | Select-Object -First 1
        if ($d) { return $d.FullName }
    }
    return $null
}

$System = @(
    'advapi32.dll', 'avicap32.dll', 'avrt.dll', 'bcrypt.dll', 'bcryptprimitives.dll', 'cfgmgr32.dll',
    'combase.dll', 'comctl32.dll', 'comdlg32.dll', 'crypt32.dll', 'd2d1.dll', 'd3d11.dll', 'd3d12.dll', 'd3d9.dll',
    'd3dcompiler_47.dll', 'dbghelp.dll', 'dcomp.dll', 'dnsapi.dll', 'dwmapi.dll', 'dwrite.dll', 'dxgi.dll',
    'gdi32.dll', 'gdiplus.dll', 'hid.dll', 'imm32.dll', 'iphlpapi.dll', 'kernel32.dll', 'ksuser.dll',
    'mf.dll', 'mfplat.dll', 'mfreadwrite.dll', 'msimg32.dll', 'msvcrt.dll', 'ncrypt.dll', 'netapi32.dll',
    'normaliz.dll', 'ntdll.dll', 'ole32.dll', 'oleacc.dll', 'oleaut32.dll', 'opengl32.dll', 'powrprof.dll',
    'propsys.dll', 'psapi.dll', 'rpcrt4.dll', 'secur32.dll', 'setupapi.dll', 'shcore.dll', 'shell32.dll',
    'shlwapi.dll', 'urlmon.dll', 'user32.dll', 'userenv.dll', 'usp10.dll', 'uxtheme.dll', 'version.dll',
    'winhttp.dll', 'wininet.dll', 'winmm.dll', 'winspool.drv', 'wldap32.dll', 'ws2_32.dll', 'wsock32.dll',
    'wtsapi32.dll', 'windowscodecs.dll', 'mswsock.dll', 'sspicli.dll', 'credui.dll', 'wintrust.dll'
)
$AlwaysForbidden = '^(vcruntime\d+(_\d+)?|msvcp\d+(_\w+)?|concrt\d+|ucrtbased|vcruntime\d+d)\.dll$'
# DLLs we ship ourselves, per importing binary.
$AppDlls = @{ 'syncwatch.exe' = @('libmpv-2.dll') }

function Get-Imports([string]$dumpbin, [string]$file) {
    $out = & $dumpbin /nologo /dependents $file 2>&1
    $imports = [System.Collections.Generic.List[pscustomobject]]::new()
    $mode = $null
    foreach ($line in $out) {
        $l = "$line"
        if ($l -match 'Image has the following dependencies') { $mode = 'normal'; continue }
        if ($l -match 'Image has the following delay load dependencies') { $mode = 'delay'; continue }
        if ($l -match '^\s*Summary') { $mode = $null; continue }
        if ($mode -and $l -match '^\s+(\S+\.(dll|drv|exe))\s*$') {
            $imports.Add([pscustomobject]@{ Name = $Matches[1].ToLowerInvariant(); Kind = $mode })
        }
    }
    return $imports
}

$dumpbin = Find-Dumpbin
if (-not $dumpbin) {
    Write-Host 'FAIL  dumpbin.exe not found. Install VS Build Tools (C++ workload) or run from the dev shell:' -ForegroundColor Red
    Write-Host '      & "D:\VS\BuildTools\Common7\Tools\Launch-VsDevShell.ps1" -Arch amd64 -HostArch amd64'
    exit 2
}

$bad = 0
foreach ($file in $Path) {
    if (-not (Test-Path $file)) { Write-Host "FAIL  $file not found (build first)" -ForegroundColor Red; $bad++; continue }
    $name = (Split-Path $file -Leaf).ToLowerInvariant()
    $imports = Get-Imports $dumpbin $file
    if ($imports.Count -eq 0) { Write-Host "FAIL  ${name}: dumpbin reported no imports (unexpected)" -ForegroundColor Red; $bad++; continue }
    $problems = @()
    foreach ($imp in $imports) {
        $n = $imp.Name
        $ok = $false
        if ($n -match $AlwaysForbidden) { $ok = $false }
        elseif ($Deny -contains $n) { $ok = $false }
        elseif ($System -contains $n -or $n -match '^(api|ext)-ms-win-[a-z0-9-]+\.dll$') { $ok = $true }
        elseif ($AppDlls.ContainsKey($name) -and $AppDlls[$name] -contains $n) { $ok = $true }
        elseif ($n -eq 'vulkan-1.dll' -and $name -eq 'libmpv-2.dll' -and -not $NoVulkanException) { $ok = $true }
        if (-not $ok) { $problems += "$n ($($imp.Kind))" }
    }
    if ($problems) {
        $bad++
        Write-Host "FAIL  $name imports non-allowed DLLs: $($problems -join ', ')" -ForegroundColor Red
    } else {
        $delay = @($imports | Where-Object Kind -eq 'delay' | ForEach-Object Name)
        $extra = if ($delay) { " (delay-load: $($delay -join ', '))" } else { '' }
        Write-Host "PASS  $name : $($imports.Count) imports, all allowed$extra" -ForegroundColor Green
    }
}
if ($bad) { exit 1 }
exit 0
