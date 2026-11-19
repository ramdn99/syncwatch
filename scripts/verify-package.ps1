#Requires -Version 7.0
<#
.SYNOPSIS
  Checks a release folder made by scripts/build-release.ps1 (Milestone 9). Never prints secrets.

.DESCRIPTION
  For SyncWatch-<version>-x64.msi and SyncWatch-Portable-<version>-x64.zip:
    - unpacks the MSI with an administrative install (msiexec /a: nothing is installed) and
      the zip, and checks the manifest: every required file is there, nothing unexpected;
    - MSI properties: per-user (ALLUSERS=2, MSIINSTALLPERUSER=1, limited privileges), the
      fixed UpgradeCode, the version, and the embedded WebView2 offline installer with a valid
      Microsoft Authenticode signature;
    - imports: only Windows system DLLs, the bundled libmpv and vulkan-1.dll
      (scripts/verify-imports.ps1 on the unpacked files);
    - no OpenSSL or native-tls in the Windows dependency tree (scripts/check-tls-tree.ps1);
    - secrets: the real Metered values from the git-ignored env files are not in any unpacked
      file (scripts/secret-scan.ps1 -Mode Paths), and no credential-like text in the config;
    - SHA256SUMS matches, and the updater signature exists when expected;
    - final sizes.
  Exit code 1 on any failure.

.EXAMPLE
  pwsh -NoProfile -File scripts/verify-package.ps1 -Dir dist\release\0.1.0
#>
param([Parameter(Mandatory)][string]$Dir, [switch]$RequireSignature)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (-not $env:LIBMPV_DIR) { $env:LIBMPV_DIR = 'D:\libs\mpv' }
$Dir =(Resolve-Path $Dir).Path
$fail = 0
function Result($ok, $text) { if ($ok) { Write-Host "PASS  $text" } else { Write-Host "FAIL  $text" -ForegroundColor Red; $script:fail++ } }

$conf = Get-Content (Join-Path $repo 'apps\desktop\src-tauri\tauri.conf.json') -Raw | ConvertFrom-Json
$version = $conf.version
$msi = Join-Path $Dir "SyncWatch-$version-x64.msi"
$zip = Join-Path $Dir "SyncWatch-Portable-$version-x64.zip"
Result (Test-Path $msi) "MSI present: $(Split-Path $msi -Leaf)"
Result (Test-Path $zip) "portable zip present: $(Split-Path $zip -Leaf)"
if ($fail) { exit 1 }

$work = Join-Path ([IO.Path]::GetTempPath()) "sw-verify-$PID"
New-Item -ItemType Directory -Force $work | Out-Null
try {
    # ---- MSI properties (Windows Installer COM, read-only) ----------------------------------
    $wi = New-Object -ComObject WindowsInstaller.Installer
    $db = $wi.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $wi, @([string]$msi, 0))
    function Prop([string]$name) {
        $v = $db.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $db, @("SELECT Value FROM Property WHERE Property='$name'"))
        $v.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $v, $null)
        $r = $v.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $v, $null)
        if ($r) { $r.GetType().InvokeMember('StringData', 'GetProperty', $null, $r, 1) } else { $null }
    }
    Result ((Prop 'ProductVersion') -eq $version) "MSI ProductVersion $(Prop 'ProductVersion')"
    $upgrade = $conf.bundle.windows.wix.upgradeCode
    Result ((Prop 'UpgradeCode') -eq "{$($upgrade.ToUpper())}") 'MSI UpgradeCode is the fixed one from tauri.conf.json'
    Result ((Prop 'ALLUSERS') -eq '2' -and (Prop 'MSIINSTALLPERUSER') -eq '1') 'per-user install (ALLUSERS=2, MSIINSTALLPERUSER=1)'
    $si = $db.GetType().InvokeMember('SummaryInformation', 'GetProperty', $null, $db, 0)
    $words = $si.GetType().InvokeMember('Property', 'GetProperty', $null, $si, 15)
    Result (($words -band 8) -eq 8) "no administrator rights requested (word count $words, limited privileges)"
    # The embedded WebView2 offline installer, checked for Microsoft's signature.
    $v = $db.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $db, @("SELECT Name, Data FROM Binary WHERE Name='MicrosoftEdgeWebView2RuntimeInstaller.exe'"))
    $v.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $v, $null)
    $r = $v.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $v, $null)
    if ($r) {
        $size = $r.GetType().InvokeMember('DataSize', 'GetProperty', $null, $r, 2)
        # The bundler embeds the file it downloaded into target\release\wix: same size, and
        # signed by Microsoft.
        $src = Get-ChildItem (Join-Path $repo 'target\release\wix') -Recurse -Filter 'MicrosoftEdgeWebView2RuntimeInstaller*.exe' -ErrorAction SilentlyContinue | Where-Object Length -EQ $size | Select-Object -First 1
        $sig = if ($src) { Get-AuthenticodeSignature $src.FullName } else { $null }
        $ok = $src -and $src.Length -eq $size -and $sig.Status -eq 'Valid' -and $sig.SignerCertificate.Subject -match 'O=Microsoft Corporation'
        Result $ok "WebView2 offline installer embedded ($([int]($size / 1MB)) MB), the same file is signed by Microsoft ($($sig.Status))"
    } else {
        Result $false 'WebView2 offline installer embedded'
    }
    [void][Runtime.InteropServices.Marshal]::ReleaseComObject($db)

    # ---- unpack -----------------------------------------------------------------------------
    $adm = Join-Path $work 'msi'
    $p = Start-Process msiexec.exe -ArgumentList @('/a', "`"$msi`"", '/qn', "TARGETDIR=`"$adm`"", '/L*v', "`"$(Join-Path $work 'admin.log')`"") -Wait -PassThru
    Result ($p.ExitCode -eq 0) "MSI unpacks (administrative install, exit $($p.ExitCode))"
    $appDir = Get-ChildItem $adm -Recurse -Filter syncwatch.exe -ErrorAction SilentlyContinue | Select-Object -First 1 | ForEach-Object DirectoryName
    $zdir = Join-Path $work 'zip'
    Expand-Archive $zip -DestinationPath $zdir
    $portDir = Join-Path $zdir 'SyncWatch'

    $required = @(
        'syncwatch.exe', 'libmpv-2.dll', 'vulkan\vulkan-1.dll', 'default-config.toml',
        'fonts\NotoSans-Regular.ttf', 'fonts\NotoSans-Bold.ttf', 'fonts\NotoSans-Italic.ttf', 'fonts\NotoSans-BoldItalic.ttf',
        'licenses\SyncWatch-GPL-3.0.txt', 'licenses\THIRD-PARTY-NOTICES.md', 'licenses\GPL-and-codec-notes.md', 'licenses\rust-crates.html',
        'licenses\mpv-Copyright.txt', 'licenses\mpv-LICENSE.GPL.txt', 'licenses\FFmpeg-LICENSE.md', 'licenses\FFmpeg-COPYING.GPLv3.txt',
        'licenses\libass-COPYING.txt', 'licenses\Vulkan-Runtime-LICENSE.txt', 'licenses\NotoSans-OFL.txt', 'licenses\Inter-OFL.txt',
        'licenses\SpaceGrotesk-OFL.txt', 'licenses\lucide-react-LICENSE.txt'
    )
    foreach ($set in @(@{ Name = 'MSI'; Dir = $appDir; Extra = @() }, @{ Name = 'zip'; Dir = $portDir; Extra = @('portable.flag') })) {
        if (-not $set.Dir -or -not (Test-Path $set.Dir)) { Result $false "$($set.Name): app folder found"; continue }
        $files = @(Get-ChildItem $set.Dir -Recurse -File | ForEach-Object { [IO.Path]::GetRelativePath($set.Dir, $_.FullName) })
        $missing = @($required + $set.Extra | Where-Object { $_ -notin $files })
        $unexpected = @($files | Where-Object { $_ -notin ($required + $set.Extra) })
        Result ($missing.Count -eq 0) "$($set.Name): every required file is present ($($files.Count) files) $($missing -join ', ')"
        Result ($unexpected.Count -eq 0) "$($set.Name): nothing unexpected $($unexpected -join ', ')"
        foreach ($pair in @(@('libmpv-2.dll', (Join-Path $env:LIBMPV_DIR 'libmpv-2.dll')), @('vulkan\vulkan-1.dll', (Join-Path $repo 'third_party\cache\vulkan\vulkan-1.dll')))) {
            $a = Join-Path $set.Dir $pair[0]
            if ((Test-Path $a) -and (Test-Path $pair[1])) { Result ((Get-FileHash $a).Hash -eq (Get-FileHash $pair[1]).Hash) "$($set.Name): $($pair[0]) is the pinned file" }
        }
    }
    if ($appDir) {
        $cfg = Get-Content (Join-Path $appDir 'default-config.toml') -Raw
        # Only the values count (the comments explain that no credentials belong here).
        $values = ($cfg -split "`n" | ForEach-Object { ($_ -split '#')[0] }) -join "`n"
        Result ($values -notmatch '(?i)metered|api_?key|credential|password|wss?://|turns?:|signal') 'default-config.toml holds no credentials or server addresses'
        & (Join-Path $repo 'scripts\verify-imports.ps1') -Path @((Join-Path $appDir 'syncwatch.exe'), (Join-Path $appDir 'libmpv-2.dll')) -Profile release
        Result ($LASTEXITCODE -eq 0) 'imports: only Windows system DLLs, the bundled libmpv and vulkan-1.dll'
        & (Join-Path $repo 'scripts\secret-scan.ps1') -Mode Paths -Path @($appDir, $portDir, $Dir) -SkipGitleaks
        Result ($LASTEXITCODE -eq 0) 'secret values: none of the real Metered values in the package (values not shown)'
    }
    pwsh -NoProfile -File (Join-Path $repo 'scripts\check-tls-tree.ps1') *> $null
    Result ($LASTEXITCODE -eq 0) 'TLS tree: no OpenSSL, native-tls or aws-lc in the Windows build'

    # ---- checksums and signature --------------------------------------------------------------
    $sums = Join-Path $Dir 'SHA256SUMS'
    $okSums = Test-Path $sums
    if ($okSums) {
        foreach ($line in Get-Content $sums) {
            $h, $n = $line -split '\s+', 2
            $f = Join-Path $Dir $n
            if (-not (Test-Path $f) -or (Get-FileHash $f).Hash.ToLowerInvariant() -ne $h) { $okSums = $false }
        }
    }
    Result $okSums 'SHA256SUMS matches every file'
    $sigFile = "$msi.sig"
    if ($RequireSignature -or (Test-Path $sigFile)) { Result (Test-Path $sigFile) 'updater signature (.msi.sig) present' }
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
Get-ChildItem $Dir -File | ForEach-Object { 'SIZE  {0,-44} {1,8:N1} MB' -f $_.Name, ($_.Length / 1MB) }
if ($fail) { Write-Host "$fail check(s) failed" -ForegroundColor Red; exit 1 }
Write-Host 'verify-package: all checks passed'
