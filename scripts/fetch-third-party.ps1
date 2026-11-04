#Requires -Version 7.0
<#
.SYNOPSIS
  Downloads and verifies the pinned third-party artifacts (fonts, Vulkan loader)
  and checks the developer's existing libmpv READ-ONLY.

.DESCRIPTION
  - Every URL and SHA-256 comes from third_party/pins.json.
  - Downloads go to third_party/cache/downloads; extracted files go to
    third_party/cache/{vulkan,fonts,licenses}. All of it is git-ignored.
  - Fonts are mirrored to apps/desktop/public/fonts (git-ignored) for the UI build.
  - Idempotent: files already present with the right hash are not downloaded again.
  - LIBMPV_DIR (default D:\libs\mpv) is only read, never written.

.EXAMPLE
  pwsh -NoProfile -File scripts/fetch-third-party.ps1
#>
[CmdletBinding()]
param(
    [string]$LibmpvDir = $(if ($env:LIBMPV_DIR) { $env:LIBMPV_DIR } else { [Environment]::GetEnvironmentVariable('LIBMPV_DIR', 'User') })
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$ProgressPreference = 'SilentlyContinue'

$root = Split-Path $PSScriptRoot -Parent
$pins = Get-Content (Join-Path $root 'third_party/pins.json') -Raw | ConvertFrom-Json
$cache = Join-Path $root 'third_party/cache'
$dl = Join-Path $cache 'downloads'
New-Item -ItemType Directory -Force $dl | Out-Null
$failed = $false

function Test-Hash([string]$Path, [string]$Expected) {
    (Test-Path $Path) -and ((Get-FileHash $Path -Algorithm SHA256).Hash -eq $Expected.ToUpperInvariant())
}

function Get-Pinned([string]$Url, [string]$Sha, [string]$Dest) {
    if (Test-Hash $Dest $Sha) { return $true }
    $tmp = "$Dest.part"
    try {
        Invoke-WebRequest -Uri $Url -OutFile $tmp -MaximumRedirection 5 -TimeoutSec 300
    } catch {
        Write-Host "FAIL  download $Url : $($_.Exception.Message)" -ForegroundColor Red
        return $false
    }
    $got = (Get-FileHash $tmp -Algorithm SHA256).Hash
    if ($got -ne $Sha.ToUpperInvariant()) {
        Remove-Item $tmp -Force
        Write-Host "FAIL  hash mismatch for $Url (expected $Sha, got $got). Not used." -ForegroundColor Red
        return $false
    }
    Move-Item $tmp $Dest -Force
    return $true
}

foreach ($d in $pins.downloads) {
    if ($d.kind -eq 'file') {
        $dest = Join-Path $cache $d.to
        New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
        if (Get-Pinned $d.url $d.sha256 $dest) { Write-Host "OK    $($d.id) -> third_party/cache/$($d.to)" }
        else { $failed = $true }
        continue
    }
    # zip: verify the archive, then extract only the listed members and verify each.
    $allPresent = $true
    foreach ($e in $d.extract) { if (-not (Test-Hash (Join-Path $cache $e.to) $e.sha256)) { $allPresent = $false } }
    if ($allPresent) { Write-Host "OK    $($d.id) (cached)"; continue }

    $zip = Join-Path $dl ([IO.Path]::GetFileName(([uri]$d.url).AbsolutePath))
    if (-not (Get-Pinned $d.url $d.sha256 $zip)) { $failed = $true; continue }
    $tmpDir = Join-Path $dl "$($d.id)-x"
    if (Test-Path $tmpDir) { Remove-Item $tmpDir -Recurse -Force }
    Expand-Archive -Path $zip -DestinationPath $tmpDir
    foreach ($e in $d.extract) {
        $src = Join-Path $tmpDir $e.from
        $dest = Join-Path $cache $e.to
        New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
        if (-not (Test-Hash $src $e.sha256)) {
            Write-Host "FAIL  $($d.id): $($e.from) missing or hash mismatch" -ForegroundColor Red
            $failed = $true; continue
        }
        Copy-Item $src $dest -Force
        Write-Host "OK    $($d.id) -> third_party/cache/$($e.to)"
    }
    Remove-Item $tmpDir -Recurse -Force
    if ($d.PSObject.Properties.Name -contains 'authenticode_signer') {
        foreach ($e in $d.extract | Where-Object { $_.to -like '*.dll' }) {
            $sig = Get-AuthenticodeSignature (Join-Path $cache $e.to)
            if ($sig.Status -ne 'Valid' -or $sig.SignerCertificate.Subject -notmatch [regex]::Escape($d.authenticode_signer)) {
                Write-Host "FAIL  $($e.to): Authenticode signature is not a valid '$($d.authenticode_signer)' signature" -ForegroundColor Red
                $failed = $true
            } else { Write-Host "OK    $($e.to) signed by $($d.authenticode_signer)" }
        }
    }
}

# Mirror fonts for the UI build (Vite serves apps/desktop/public as /).
$pubFonts = Join-Path $root 'apps/desktop/public/fonts'
New-Item -ItemType Directory -Force $pubFonts | Out-Null
Get-ChildItem (Join-Path $cache 'fonts') -File -ErrorAction SilentlyContinue | ForEach-Object {
    $t = Join-Path $pubFonts $_.Name
    if (-not (Test-Path $t) -or (Get-FileHash $t).Hash -ne (Get-FileHash $_.FullName).Hash) { Copy-Item $_.FullName $t -Force }
}
Write-Host 'OK    fonts mirrored to apps/desktop/public/fonts'

# libmpv: read-only verification.
if (-not $LibmpvDir) {
    Write-Host 'FAIL  LIBMPV_DIR is not set. Set it at user level: [Environment]::SetEnvironmentVariable("LIBMPV_DIR", "D:\libs\mpv", "User")' -ForegroundColor Red
    $failed = $true
} else {
    foreach ($p in $pins.libmpv.files.PSObject.Properties) {
        $f = Join-Path $LibmpvDir $p.Name
        if (Test-Hash $f $p.Value) { Write-Host "OK    libmpv $($p.Name) matches $($pins.libmpv.build)" }
        elseif (Test-Path $f) { Write-Host "FAIL  libmpv $($p.Name) differs from the pinned $($pins.libmpv.build). Update third_party/pins.json and docs/libmpv-build.md deliberately if you changed builds." -ForegroundColor Red; $failed = $true }
        else { Write-Host "FAIL  libmpv $($p.Name) missing in $LibmpvDir" -ForegroundColor Red; $failed = $true }
    }
    foreach ($h in $pins.libmpv.hand_generated) {
        if (-not (Test-Path (Join-Path $LibmpvDir $h))) { Write-Host "FAIL  $h missing in $LibmpvDir (see docs/libmpv-build.md to generate it)" -ForegroundColor Red; $failed = $true }
    }
}

if ($failed) { Write-Host 'fetch-third-party: FAILED' -ForegroundColor Red; exit 1 }
Write-Host 'fetch-third-party: all pinned artifacts present and verified.' -ForegroundColor Green
exit 0
