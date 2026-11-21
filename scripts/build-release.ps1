#Requires -Version 7.0
<#
.SYNOPSIS
  The only supported release build (Milestone 9): SyncWatch-<version>-x64.msi, the portable
  zip, SHA256SUMS and (with an updater key) the update signature, all checked by
  scripts/verify-package.ps1.

.DESCRIPTION
  1. Verifies every pinned third-party file (fetch-third-party.ps1).
  2. Stages the runtime files in target\release-stage: libmpv-2.dll, vulkan\vulkan-1.dll,
     fonts\NotoSans-*.ttf, default-config.toml, licenses\ (SyncWatch GPL-3.0, third-party
     notices, mpv/FFmpeg/libass, fonts, Vulkan, Lucide, and the Rust crate report from
     cargo-about).
  3. Runs `tauri build --bundles msi` with a generated config overlay that adds those files as
     resources (per-user MSI template in src-tauri\wix, WebView2 offline installer embedded).
  4. Writes, under dist\release\<version>:
       SyncWatch-<version>-x64.msi
       SyncWatch-Portable-<version>-x64.zip  (an extra artifact; not attached to releases)
       SyncWatch-<version>-x64.msi.sig       (only with -UpdaterKey: Tauri updater signature)
       SHA256SUMS
       logs\  (the full output of every step; kept when something fails)
  5. Runs scripts/verify-package.ps1 on the result.

  The updater key password is read from TAURI_SIGNING_PRIVATE_KEY_PASSWORD if set, otherwise
  prompted for (never echoed, never written anywhere). This script never prints a secret.

.EXAMPLE
  pwsh -NoProfile -File scripts/build-release.ps1
  -TestBuild (tests only, never for a release): compiles in SYNCWATCH_TEST_BUILD, which lets
  the app accept a local update server and a test key; writes to dist\test\<version> with a
  TEST-BUILD.txt marker (scripts/release.ps1 refuses such folders). -Version overrides the
  version for such test builds (for example 0.0.1 and 0.0.2 for the update test).

.EXAMPLE
  pwsh -NoProfile -File scripts/build-release.ps1 -UpdaterKey D:\keys\syncwatch-updater.key
.EXAMPLE
  pwsh -NoProfile -File scripts/build-release.ps1 -TestBuild -Version 0.0.1 -UpdaterKey <test key>
#>
param(
    [string]$UpdaterKey = '',
    [string]$OutRoot = '',
    [string]$Version = '',
    [switch]$TestBuild,
    [switch]$SkipVerify
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo
foreach ($kv in @{ RUSTUP_HOME = 'D:\Rust\rustup'; CARGO_HOME = 'D:\Rust\cargo'; LIBMPV_DIR = 'D:\libs\mpv' }.GetEnumerator()) {
    if (-not [Environment]::GetEnvironmentVariable($kv.Key)) { Set-Item "Env:$($kv.Key)" $kv.Value }
}
$env:PATH = "$env:CARGO_HOME\bin;$env:PATH"
$pnpm = if (Get-Command pnpm.cmd -ErrorAction SilentlyContinue) { 'pnpm.cmd' } else { 'pnpm' }

$conf = Get-Content apps\desktop\src-tauri\tauri.conf.json -Raw | ConvertFrom-Json
$version = if ($Version) { $Version } else { $conf.version }
if ($Version -and -not $TestBuild -and $Version -ne $conf.version) {
    Write-Host "A release uses the version in tauri.conf.json ($($conf.version)); -Version is for -TestBuild only." -ForegroundColor Red
    exit 1
}
if (-not $OutRoot) { $OutRoot = Join-Path $repo $(if ($TestBuild) { 'dist\test' } else { 'dist\release' }) }
$out = Join-Path $OutRoot $version
$logs = Join-Path $out 'logs'
# Empty the folder rather than delete it: a virus scanner or the search indexer may still hold
# the folder itself open for a moment after the previous build.
if (Test-Path $out) { Get-ChildItem $out -Force | Remove-Item -Recurse -Force }
New-Item -ItemType Directory -Force $logs | Out-Null

# Runs a step, keeps its full output in logs\<name>.log, stops with the tail on failure.
function Step([string]$name, [scriptblock]$cmd) {
    $log = Join-Path $logs "$name.log"
    Write-Host "== $name"
    $t = Get-Date
    & $cmd *>&1 | Out-File -Encoding utf8 $log
    $code = $LASTEXITCODE
    if ($code) {
        Write-Host "FAIL  $name (exit $code). Full log: $log" -ForegroundColor Red
        Get-Content $log -Tail 30 | ForEach-Object { "    $_" }
        exit 1
    }
    Write-Host ("   ok in {0:N0} s" -f ((Get-Date) - $t).TotalSeconds)
}

Step 'fetch-third-party' { pwsh -NoProfile -File scripts/fetch-third-party.ps1 }

# ---- stage ---------------------------------------------------------------------------------
$stage = Join-Path $repo 'target\release-stage'
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
foreach ($d in 'vulkan', 'fonts', 'licenses') { New-Item -ItemType Directory -Force (Join-Path $stage $d) | Out-Null }
$cache = Join-Path $repo 'third_party\cache'
Copy-Item (Join-Path $env:LIBMPV_DIR 'libmpv-2.dll') $stage
Copy-Item (Join-Path $cache 'vulkan\vulkan-1.dll') (Join-Path $stage 'vulkan')
Get-ChildItem (Join-Path $cache 'fonts') -Filter 'NotoSans-*.ttf' | Copy-Item -Destination (Join-Path $stage 'fonts')
Copy-Item apps\desktop\src-tauri\default-config.toml $stage
$lic = Join-Path $stage 'licenses'
Copy-Item LICENSE (Join-Path $lic 'SyncWatch-GPL-3.0.txt')
Copy-Item THIRD-PARTY-NOTICES.md $lic
Copy-Item docs\legal-notes.md (Join-Path $lic 'GPL-and-codec-notes.md')
Get-ChildItem (Join-Path $cache 'licenses') -File | Copy-Item -Destination $lic
Copy-Item third_party\licenses\lucide-react-LICENSE.txt $lic
Step 'cargo-about' { cargo about generate --manifest-path apps/desktop/src-tauri/Cargo.toml about.hbs -o (Join-Path $lic 'rust-crates.html') }

# ---- tauri build -----------------------------------------------------------------------------
$resources = [ordered]@{}
Get-ChildItem $stage -Recurse -File | ForEach-Object {
    $rel = [IO.Path]::GetRelativePath($stage, $_.FullName).Replace('\', '/')
    $resources[$_.FullName.Replace('\', '/')] = $rel
}
$overlay = [ordered]@{ version = $version; bundle = [ordered]@{ active = $true; resources = $resources } }
$signing = $UpdaterKey -ne ''
if ($signing -and -not (Test-Path $UpdaterKey)) { Write-Host "Updater key not found: $UpdaterKey" -ForegroundColor Red; exit 1 }
$overlayPath = Join-Path $stage 'tauri.release.json'
$overlay | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 $overlayPath
# Test builds also bundle the dev scenarios into the UI (VITE_SYNCWATCH_TEST_BUILD).
if ($TestBuild) { $env:SYNCWATCH_TEST_BUILD = '1'; $env:VITE_SYNCWATCH_TEST_BUILD = '1' } else { Remove-Item Env:SYNCWATCH_TEST_BUILD, Env:VITE_SYNCWATCH_TEST_BUILD -ErrorAction SilentlyContinue }
try {
    Step 'tauri-build' { & $pnpm --dir apps/desktop tauri build --bundles msi --config $overlayPath }
} finally {
    Remove-Item Env:SYNCWATCH_TEST_BUILD, Env:VITE_SYNCWATCH_TEST_BUILD -ErrorAction SilentlyContinue
}

$msiDir = Join-Path $repo 'target\release\bundle\msi'
$built = Get-ChildItem $msiDir -Filter "*_${version}_x64_*.msi" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $built) { Write-Host "FAIL  no MSI for $version in $msiDir" -ForegroundColor Red; exit 1 }
$msiName = "SyncWatch-$version-x64.msi"
Copy-Item $built.FullName (Join-Path $out $msiName)
if ($TestBuild) { "Test build $version (SYNCWATCH_TEST_BUILD): never publish." | Set-Content (Join-Path $out 'TEST-BUILD.txt') }
if ($signing) {
    # The updater signature (minisign, Tauri's format). The password comes from
    # TAURI_SIGNING_PRIVATE_KEY_PASSWORD or a prompt; it is never put on a command line.
    $askedHere = -not $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD
    if ($askedHere) {
        $sec = Read-Host -AsSecureString 'Updater key password'
        $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = [Net.NetworkCredential]::new('', $sec).Password
    }
    try {
        Step 'sign-msi' { & $pnpm --dir apps/desktop tauri signer sign -f $UpdaterKey --app-version $version (Join-Path $out $msiName) }
    } finally {
        if ($askedHere) { Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue }
    }
    if (-not (Test-Path (Join-Path $out "$msiName.sig"))) { Write-Host 'FAIL  the updater signature was not created' -ForegroundColor Red; exit 1 }
}

# ---- portable zip ------------------------------------------------------------------------------
$port = Join-Path $stage 'portable\SyncWatch'
New-Item -ItemType Directory -Force $port | Out-Null
Copy-Item target\release\syncwatch.exe $port
Get-ChildItem $stage -Exclude 'portable', 'tauri.release.json' | Copy-Item -Destination $port -Recurse
New-Item -ItemType File (Join-Path $port 'portable.flag') | Out-Null
$zipName = "SyncWatch-Portable-$version-x64.zip"
Compress-Archive -Path $port -DestinationPath (Join-Path $out $zipName) -CompressionLevel Optimal

# ---- checksums -----------------------------------------------------------------------------------
$sums = Get-ChildItem $out -File | Where-Object Name -NE 'SHA256SUMS' | Sort-Object Name | ForEach-Object {
    '{0}  {1}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $_.Name
}
$sums | Set-Content -Encoding ascii (Join-Path $out 'SHA256SUMS')

Get-ChildItem $out -File | ForEach-Object { '{0,-44} {1,8:N1} MB' -f $_.Name, ($_.Length / 1MB) }
if (-not $SkipVerify) {
    pwsh -NoProfile -File scripts/verify-package.ps1 -Dir $out -Version $version $(if ($signing) { '-RequireSignature' })
    if ($LASTEXITCODE) { exit 1 }
}
Write-Host "Release files: $out"
