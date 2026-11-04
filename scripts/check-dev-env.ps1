#Requires -Version 7.0
<#
.SYNOPSIS
  Checks the developer environment for SyncWatch up to a given milestone.

.DESCRIPTION
  Each check belongs to the first milestone that needs it. Checks for milestones up to
  -Milestone FAIL when missing; checks for later milestones only WARN.
  Tools are found through the process PATH, the user/machine PATH stored in the registry
  (handles terminals started before PATH changed), and known install locations.
  Never prints secrets: env files are only checked for presence.

.EXAMPLE
  pwsh -NoProfile -File scripts/check-dev-env.ps1 -Milestone 1
#>
[CmdletBinding()]
param(
    [ValidateRange(1, 9)]
    [int]$Milestone = 1
)

if ($PSVersionTable.PSVersion.Major -lt 7) {
    Write-Host 'This script needs PowerShell 7. Run it with: pwsh -NoProfile -File scripts/check-dev-env.ps1 -Milestone 1'
    exit 2
}
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path $PSScriptRoot -Parent
$results = [System.Collections.Generic.List[pscustomobject]]::new()

function Add-Result([int]$m, [string]$name, [bool]$ok, [string]$detail, [string]$fix = '') {
    $status = if ($ok) { 'PASS' } elseif ($m -le $Milestone) { 'FAIL' } else { 'WARN' }
    $results.Add([pscustomobject]@{ Status = $status; M = $m; Name = $name; Detail = $detail; Fix = $fix })
}

function Get-RegistryPathDirs {
    $dirs = @()
    foreach ($scope in 'User', 'Machine') {
        $p = [Environment]::GetEnvironmentVariable('Path', $scope)
        if ($p) { $dirs += $p -split ';' | Where-Object { $_ } }
    }
    $dirs
}
$RegistryPath = Get-RegistryPathDirs

# Returns @{ Path; Where } or $null. Where = 'PATH' | 'registry PATH (restart terminal)' | 'known location (not on PATH)'
function Find-Tool([string]$exe, [string[]]$known = @()) {
    $c = Get-Command $exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($c) { return @{ Path = $c.Source; Where = 'PATH' } }
    foreach ($d in $RegistryPath) {
        $p = Join-Path ([Environment]::ExpandEnvironmentVariables($d)) $exe
        if (Test-Path $p) { return @{ Path = $p; Where = 'registry PATH (open a new terminal to pick it up)' } }
    }
    foreach ($k in $known) { if (Test-Path $k) { return @{ Path = $k; Where = 'known location, NOT on PATH' } } }
    return $null
}

function Get-EnvAny([string]$name) {
    $v = [Environment]::GetEnvironmentVariable($name)
    if ($v) { return @{ Value = $v; Where = 'process' } }
    $u = [Environment]::GetEnvironmentVariable($name, 'User')
    if ($u) { return @{ Value = $u; Where = 'user (open a new terminal to pick it up)' } }
    return $null
}

function Tool-Check([int]$m, [string]$name, [string]$exe, [string[]]$known, [string]$fix, [scriptblock]$versionCheck = $null) {
    $t = Find-Tool $exe $known
    if (-not $t) { Add-Result $m $name $false 'not found' $fix; return $null }
    $detail = "$($t.Path) [$($t.Where)]"
    $ok = $true
    if ($versionCheck) {
        $r = & $versionCheck $t.Path
        $detail = "$($r.Text) - $detail"
        $ok = $r.Ok
    }
    Add-Result $m $name $ok $detail $(if ($ok) { '' } else { $fix })
    return $t
}

$pathFix = { param($dir) "Add to your user PATH (one time), then open a new terminal:`n      [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path','User') + ';$dir', 'User')" }

# ---------------- Milestone 1 ----------------
Add-Result 1 'PowerShell 7' $true "pwsh $($PSVersionTable.PSVersion)"
Tool-Check 1 'Git' 'git.exe' @('D:\Program Files\Git\cmd\git.exe') 'Install Git for Windows to D:\Program Files\Git.' | Out-Null

$rustupHome = Get-EnvAny 'RUSTUP_HOME'
$cargoHome = Get-EnvAny 'CARGO_HOME'
Add-Result 1 'RUSTUP_HOME' ([bool]$rustupHome -and $rustupHome.Value -eq 'D:\Rust\rustup') $(if ($rustupHome) { "$($rustupHome.Value) [$($rustupHome.Where)]" } else { 'not set' }) "[Environment]::SetEnvironmentVariable('RUSTUP_HOME', 'D:\Rust\rustup', 'User')"
Add-Result 1 'CARGO_HOME' ([bool]$cargoHome -and $cargoHome.Value -eq 'D:\Rust\cargo') $(if ($cargoHome) { "$($cargoHome.Value) [$($cargoHome.Where)]" } else { 'not set' }) "[Environment]::SetEnvironmentVariable('CARGO_HOME', 'D:\Rust\cargo', 'User')"
if (-not $env:RUSTUP_HOME -and $rustupHome) { $env:RUSTUP_HOME = $rustupHome.Value }
if (-not $env:CARGO_HOME -and $cargoHome) { $env:CARGO_HOME = $cargoHome.Value }

$wantRust = ((Get-Content (Join-Path $root 'rust-toolchain.toml') -Raw) -match 'channel\s*=\s*"([^"]+)"') ? $Matches[1] : $null
$cargo = Tool-Check 1 'cargo' 'cargo.exe' @('D:\Rust\cargo\bin\cargo.exe') (& $pathFix 'D:\Rust\cargo\bin')
$rustc = Find-Tool 'rustc.exe' @('D:\Rust\cargo\bin\rustc.exe')
if ($rustc) {
    Push-Location $root
    try { $v = (& $rustc.Path --version 2>&1 | Out-String).Trim() } finally { Pop-Location }
    Add-Result 1 "rustc $wantRust (rust-toolchain.toml)" ($v -match [regex]::Escape($wantRust)) $v "rustup toolchain install $wantRust --component clippy rustfmt"
    $rustup = Find-Tool 'rustup.exe' @('D:\Rust\cargo\bin\rustup.exe')
    if ($rustup) {
        Push-Location $root
        try { $comps = & $rustup.Path component list --installed 2>&1 | Out-String } finally { Pop-Location }
        Add-Result 1 'clippy + rustfmt' ($comps -match 'clippy' -and $comps -match 'rustfmt') 'installed components checked' 'rustup component add clippy rustfmt'
        Add-Result 1 'target x86_64-pc-windows-msvc' ($comps -match 'rust-std-x86_64-pc-windows-msvc') '' 'rustup target add x86_64-pc-windows-msvc'
    }
} else { Add-Result 1 'rustc' $false 'not found' (& $pathFix 'D:\Rust\cargo\bin') }

$vsRoot = 'D:\VS\BuildTools'
$devShell = Join-Path $vsRoot 'Common7\Tools\Launch-VsDevShell.ps1'
Add-Result 1 'VS Build Tools (dev shell)' (Test-Path $devShell) $devShell 'Install Visual Studio Build Tools with the "Desktop development with C++" workload to D:\VS\BuildTools.'
$dumpbin = Get-ChildItem (Join-Path $vsRoot 'VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe') -ErrorAction SilentlyContinue | Select-Object -Last 1
Add-Result 1 'MSVC x64 tools (dumpbin, lib)' ([bool]$dumpbin) $(if ($dumpbin) { $dumpbin.FullName } else { 'not found' }) 'VS Installer > Modify Build Tools > MSVC v143 x64/x86 build tools.'
$sdk = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\Lib' -Directory -ErrorAction SilentlyContinue | Sort-Object Name | Select-Object -Last 1
Add-Result 1 'Windows 10/11 SDK' ([bool]$sdk) $(if ($sdk) { $sdk.Name } else { 'not found' }) 'VS Installer > Modify Build Tools > Windows 11 SDK.'

Tool-Check 1 'Node.js >= 22' 'node.exe' @('D:\Program Files\nodejs\node.exe') 'Install Node.js LTS to D:\Program Files\nodejs.' {
    param($p) $v = (& $p --version).Trim(); @{ Ok = ([version]($v.TrimStart('v')) -ge [version]'22.0.0'); Text = $v }
} | Out-Null
Tool-Check 1 'pnpm 12' 'pnpm.cmd' @("$env:APPDATA\npm\pnpm.cmd") "npm install -g pnpm@12  (if script execution is blocked, call pnpm.cmd instead of pnpm)" {
    param($p) $v = (& $p --version).Trim(); @{ Ok = $v.StartsWith('12.'); Text = "pnpm $v" }
} | Out-Null

$libmpv = Get-EnvAny 'LIBMPV_DIR'
Add-Result 1 'LIBMPV_DIR' ([bool]$libmpv) $(if ($libmpv) { "$($libmpv.Value) [$($libmpv.Where)]" } else { 'not set' }) "[Environment]::SetEnvironmentVariable('LIBMPV_DIR', 'D:\libs\mpv', 'User')"
if ($libmpv) {
    $pins = Get-Content (Join-Path $root 'third_party/pins.json') -Raw | ConvertFrom-Json
    foreach ($f in @('libmpv-2.dll', 'mpv.lib', 'include\mpv\client.h')) {
        Add-Result 1 "libmpv file $f" (Test-Path (Join-Path $libmpv.Value $f)) (Join-Path $libmpv.Value $f) 'See docs/libmpv-build.md (mpv.lib is generated with dumpbin and lib from the dev shell).'
    }
    $dll = Join-Path $libmpv.Value 'libmpv-2.dll'
    if (Test-Path $dll) {
        $h = (Get-FileHash $dll).Hash
        Add-Result 1 'libmpv build matches pins.json' ($h -eq $pins.libmpv.files.'libmpv-2.dll') "$($pins.libmpv.build)" 'Your libmpv differs from third_party/pins.json. Update pins and docs/libmpv-build.md deliberately.'
    }
}

$wv = $null
foreach ($k in 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'HKCU:\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}') {
    $v = (Get-ItemProperty $k -ErrorAction SilentlyContinue).pv
    if ($v -and $v -ne '0.0.0.0') { $wv = $v; break }
}
Add-Result 1 'WebView2 Runtime' ([bool]$wv) $(if ($wv) { $wv } else { 'not installed' }) 'Install the Evergreen WebView2 Runtime from Microsoft.'

$cache = Join-Path $root 'third_party/cache'
Add-Result 1 'third-party cache (fonts, Vulkan loader)' ((Test-Path (Join-Path $cache 'vulkan/vulkan-1.dll')) -and (Test-Path (Join-Path $cache 'fonts/SpaceGrotesk-Bold.ttf'))) $cache 'pwsh -NoProfile -File scripts/fetch-third-party.ps1'
Add-Result 1 'node_modules installed' (Test-Path (Join-Path $root 'node_modules/.pnpm')) '' 'pnpm.cmd install'

Tool-Check 1 'ffprobe (test video check)' 'ffprobe.exe' @("$env:LOCALAPPDATA\Microsoft\WinGet\Links\ffprobe.exe") 'winget install Gyan.FFmpeg' | Out-Null
Add-Result 1 'Test video D:\media\test.mkv' (Test-Path 'D:\media\test.mkv') 'D:\media\test.mkv' 'Place a 20+ minute MKV with 2+ audio and 2+ subtitle tracks at D:\media\test.mkv.'
Tool-Check 1 '7-Zip (inspect libmpv archives)' '7z.exe' @('D:\Program Files\7-Zip\7z.exe') (& $pathFix 'D:\Program Files\7-Zip') | Out-Null
Tool-Check 1 'MKVToolNix (test file prep)' 'mkvmerge.exe' @('D:\Program Files\MKVToolNix\mkvmerge.exe') (& $pathFix 'D:\Program Files\MKVToolNix') | Out-Null

$docker = Tool-Check 1 'Docker (gitleaks secret scan)' 'docker.exe' @("$env:LOCALAPPDATA\Programs\DockerDesktop\resources\bin\docker.exe") 'Install Docker Desktop; it provides the pinned gitleaks image used by scripts/secret-scan.ps1.'
if ($docker) {
    & $docker.Path info *> $null
    Add-Result 4 'Docker engine running' ($LASTEXITCODE -eq 0) '' 'Start Docker Desktop (needed for the secret scan hook and from Milestone 4 for signaling + coturn).'
}
Add-Result 1 'git hooks enabled (secret scan)' ((git -C $root config core.hooksPath) -eq 'scripts/git-hooks') '' 'git config core.hooksPath scripts/git-hooks'
Add-Result 1 '.env.local present (dev credentials; content not read)' (Test-Path (Join-Path $root '.env.local')) '' 'Create .env.local with METERED_APP and METERED_API_KEY (git-ignored).'

# ---------------- later milestones (WARN until reached) ----------------
Tool-Check 3 'LLVM/clang (crypto crates)' 'clang.exe' @('D:\Program Files\LLVM\bin\clang.exe') (& $pathFix 'D:\Program Files\LLVM\bin') | Out-Null
Tool-Check 3 'CMake' 'cmake.exe' @('D:\Program Files\CMake\bin\cmake.exe') (& $pathFix 'D:\Program Files\CMake\bin') | Out-Null
Tool-Check 3 'NASM' 'nasm.exe' @('C:\Users\ramdn\AppData\Local\bin\NASM\nasm.exe') (& $pathFix 'C:\Users\ramdn\AppData\Local\bin\NASM') | Out-Null
Add-Result 3 '.env.ice.local present (content not read)' (Test-Path (Join-Path $root '.env.ice.local')) '' 'Paste your Metered ICE servers array into .env.ice.local (git-ignored).'
Tool-Check 7 'qBittorrent (torrent test seeder)' 'qbittorrent.exe' @('C:\Program Files\qBittorrent\qbittorrent.exe', 'D:\Program Files\qBittorrent\qbittorrent.exe') 'Install qBittorrent at the start of Milestone 7.' | Out-Null
Tool-Check 9 'cargo-about' 'cargo-about.exe' @('D:\Rust\cargo\bin\cargo-about.exe') 'cargo install cargo-about --locked' | Out-Null
Tool-Check 9 'cargo-deny' 'cargo-deny.exe' @('D:\Rust\cargo\bin\cargo-deny.exe') 'cargo install cargo-deny --locked' | Out-Null
Tool-Check 9 'cargo-audit' 'cargo-audit.exe' @('D:\Rust\cargo\bin\cargo-audit.exe') 'cargo install cargo-audit --locked' | Out-Null

# ---------------- report ----------------
$color = @{ PASS = 'Green'; WARN = 'Yellow'; FAIL = 'Red' }
foreach ($r in $results | Sort-Object M) {
    $line = '{0}  [M{1}] {2}' -f $r.Status, $r.M, $r.Name
    if ($r.Detail) { $line += " - $($r.Detail)" }
    Write-Host $line -ForegroundColor $color[$r.Status]
    if ($r.Status -ne 'PASS' -and $r.Fix) { Write-Host "      fix: $($r.Fix)" }
}
$fails = @($results | Where-Object Status -eq 'FAIL').Count
$warns = @($results | Where-Object Status -eq 'WARN').Count
Write-Host ''
Write-Host "check-dev-env -Milestone ${Milestone}: $fails FAIL, $warns WARN, $(@($results | Where-Object Status -eq 'PASS').Count) PASS"
if ($fails) { exit 1 }
exit 0
