#Requires -Version 7.0
<#
.SYNOPSIS
  Publishes a SyncWatch release to GitHub (run by the maintainer in their own terminal).

.DESCRIPTION
  In order, stopping at the first problem:
    1.  Tools and login: git, the GitHub CLI (`gh auth status`; it never asks for a token).
    2.  Repository: on main, clean working tree (CLAUDE.md, local notes, is ignored), in step
        with origin/main, and the tag vX.Y.Z and the release do not exist yet.
    3.  Version: the same X.Y.Z in Cargo.toml, apps/desktop/package.json and tauri.conf.json,
        and a "## X.Y.Z" section in CHANGELOG.md (used as the release notes).
    4.  Updater key: the private key file exists; its .pub file next to it matches
        apps/desktop/src-tauri/updater-pubkey.txt (the key the app trusts).
    5.  Public-repo safety: the full-history secret scan (gitleaks + your real Metered values,
        never shown) and the repository visibility.
    6.  The full CI script (scripts/ci.ps1).
    7.  scripts/build-release.ps1 with the updater key: MSI, its signature, portable zip,
        SHA256SUMS, and scripts/verify-package.ps1.
    8.  latest.json (version, notes, date, MSI URL, SHA-256, signature) and its signature;
        SHA256SUMS again with every file.
    9.  Tag vX.Y.Z (annotated), push the tag, create the GitHub release with the notes and
        upload: the MSI, its .sig, latest.json, latest.json.sig and SHA256SUMS. The portable zip
        is NOT attached (add -AttachPortable to attach it).
    10. Checks afterwards: every asset is reachable, and latest.json from the "latest" link
        parses and matches the MSI's SHA-256. Prints the release URL.

  The key password is asked once (hidden) and kept only in this process's environment for the
  signing steps. Nothing secret is printed or written to a file.

  -DryRun does everything except tagging, pushing and uploading. Without -UpdaterKey a dry run
  makes a throwaway key in a temporary folder (and deletes it afterwards).

.EXAMPLE
  pwsh -NoProfile -File scripts/release.ps1 -Version 0.1.0 -UpdaterKey D:\keys\syncwatch-updater.key -DryRun
.EXAMPLE
  pwsh -NoProfile -File scripts/release.ps1 -Version 0.1.0 -UpdaterKey D:\keys\syncwatch-updater.key
#>
param(
    [Parameter(Mandatory)][string]$Version,
    [string]$UpdaterKey = '',
    [switch]$DryRun,
    [switch]$AttachPortable,
    [switch]$SkipCi
)
$ErrorActionPreference = 'Stop'
$Repo = 'ramdn99/syncwatch'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
$tag = "v$Version"
$pnpm = if (Get-Command pnpm.cmd -ErrorAction SilentlyContinue) { 'pnpm.cmd' } else { 'pnpm' }
function Ok([string]$t) { Write-Host "OK    $t" }
function Stop-Release([string]$t) { Write-Host "STOP  $t" -ForegroundColor Red; exit 1 }
function Step([string]$t) { Write-Host ''; Write-Host "== $t" -ForegroundColor Cyan }
$mode = if ($DryRun) { 'DRY RUN (no tag, no push, no upload)' } else { 'REAL RELEASE' }
Write-Host "SyncWatch release $Version - $mode"

# 1. tools ----------------------------------------------------------------------------------
Step 'Tools and GitHub login'
foreach ($c in 'git', 'gh', $pnpm) { if (-not (Get-Command $c -ErrorAction SilentlyContinue)) { Stop-Release "$c not found" } }
gh auth status *> $null
if ($LASTEXITCODE) { Stop-Release 'GitHub CLI is not logged in. Run: gh auth login (in your own terminal)' }
Ok 'gh is logged in'

# 2. repository -----------------------------------------------------------------------------
Step 'Repository state'
if ($Version -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$') { Stop-Release 'Version must look like 0.1.0' }
$branch = (git rev-parse --abbrev-ref HEAD).Trim()
if ($branch -ne 'main') { Stop-Release "on branch $branch, not main" }
$dirty = @(git status --porcelain | Where-Object { $_ -notmatch ' CLAUDE\.md$' })
if ($dirty.Count) { $dirty | ForEach-Object { "      $_" }; Stop-Release 'the working tree is not clean (commit or stash first)' }
Ok 'on main, working tree clean'
git fetch --quiet origin main --tags
if ((git rev-parse HEAD).Trim() -ne (git rev-parse origin/main).Trim()) {
    if (-not $DryRun) { Stop-Release 'main is not the same as origin/main (push your commits first, or pull)' }
    Write-Host 'WARN  main differs from origin/main (fine for a dry run; push before the real release)' -ForegroundColor Yellow
} else {
    Ok 'main matches origin/main'
}
if (git tag --list $tag) { Stop-Release "tag $tag already exists" }
if (git ls-remote --tags origin "refs/tags/$tag") { Stop-Release "tag $tag already exists on GitHub" }
gh release view $tag --repo $Repo *> $null
if (-not $LASTEXITCODE) { Stop-Release "release $tag already exists on GitHub" }
Ok "tag and release $tag are new"

# 3. version and changelog -------------------------------------------------------------------
Step 'Version and changelog'
$cargoV = ([regex]::Match((Get-Content Cargo.toml -Raw), '(?m)^\[workspace\.package\][\s\S]*?^version = "([^"]+)"')).Groups[1].Value
$pkgV = (Get-Content apps\desktop\package.json -Raw | ConvertFrom-Json).version
$confV = (Get-Content apps\desktop\src-tauri\tauri.conf.json -Raw | ConvertFrom-Json).version
foreach ($p in @(@('Cargo.toml', $cargoV), @('apps/desktop/package.json', $pkgV), @('tauri.conf.json (and the MSI)', $confV))) {
    if ($p[1] -ne $Version) { Stop-Release "$($p[0]) says $($p[1]), not $Version (run scripts/bump-version.ps1 -Version $Version)" }
}
Ok "version $Version everywhere"
$log = Get-Content CHANGELOG.md -Raw
$m = [regex]::Match($log, "(?ms)^## $([regex]::Escape($Version))\b[^\n]*\n(.*?)(?=^## |\z)")
if (-not $m.Success -or -not $m.Groups[1].Value.Trim()) { Stop-Release "CHANGELOG.md has no '## $Version' section with text" }
$notes = $m.Groups[1].Value.Trim()
Ok "changelog section found ($($notes.Length) characters)"

# 4. updater key -----------------------------------------------------------------------------
Step 'Updater key'
$tempKeyDir = $null
if (-not $UpdaterKey) {
    if (-not $DryRun) { Stop-Release 'a real release needs -UpdaterKey <path to your private key>' }
    $tempKeyDir = Join-Path ([IO.Path]::GetTempPath()) "sw-dryrun-key-$PID"
    New-Item -ItemType Directory -Force $tempKeyDir | Out-Null
    $UpdaterKey = Join-Path $tempKeyDir 'dryrun.key'
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = [Convert]::ToBase64String([Security.Cryptography.RandomNumberGenerator]::GetBytes(24))
    & $pnpm --dir apps/desktop tauri signer generate -w $UpdaterKey -p $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD --ci -f *> $null
    if (-not (Test-Path $UpdaterKey)) { Stop-Release 'could not make the throwaway dry-run key' }
    Ok 'dry run: a throwaway key in a temporary folder (deleted at the end)'
}
if (-not (Test-Path $UpdaterKey)) { Stop-Release "key file not found: $UpdaterKey" }
$appKey = (Get-Content apps\desktop\src-tauri\updater-pubkey.txt -Raw).Trim()
$pub = "$UpdaterKey.pub"
if ($appKey -eq 'REPLACE_WITH_UPDATER_PUBLIC_KEY') {
    if (-not $DryRun) { Stop-Release 'apps/desktop/src-tauri/updater-pubkey.txt still has the placeholder: put your public key there and commit it (release guide, step a)' }
    Write-Host 'WARN  updater-pubkey.txt holds the placeholder: this dry-run build could not verify updates' -ForegroundColor Yellow
} elseif (-not (Test-Path $pub)) {
    Stop-Release "public key $pub not found next to the private key (it must match updater-pubkey.txt)"
} elseif ((Get-Content $pub -Raw).Trim() -ne $appKey) {
    if (-not $DryRun) { Stop-Release "the key's public part does not match updater-pubkey.txt: updates signed with it would be refused" }
    Write-Host 'WARN  dry-run key differs from updater-pubkey.txt (expected for a throwaway key)' -ForegroundColor Yellow
} else {
    Ok 'the key matches the public key in the app'
}
if (-not $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) {
    $sec = Read-Host -AsSecureString 'Updater key password (hidden)'
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = [Net.NetworkCredential]::new('', $sec).Password
}

try {
    # 5. public-repo safety -------------------------------------------------------------------
    Step 'Public repository safety'
    pwsh -NoProfile -File scripts/secret-scan.ps1 -Mode History
    if ($LASTEXITCODE) { Stop-Release 'the history secret scan found something (see above)' }
    $vis = (gh repo view $Repo --json visibility --jq .visibility).Trim()
    Ok "repository visibility: $vis"

    # 6. CI -------------------------------------------------------------------------------------
    if ($SkipCi -and -not $DryRun) { Stop-Release '-SkipCi is only for dry runs' }
    if (-not $SkipCi) {
        Step 'CI (scripts/ci.ps1)'
        pwsh -NoProfile -File scripts/ci.ps1
        if ($LASTEXITCODE) { Stop-Release 'CI failed' }
        Ok 'CI green'
    }

    # 7. build ------------------------------------------------------------------------------------
    Step 'Release build (scripts/build-release.ps1)'
    pwsh -NoProfile -File scripts/build-release.ps1 -UpdaterKey $UpdaterKey
    if ($LASTEXITCODE) { Stop-Release 'the release build failed (logs in dist\release\<version>\logs)' }
    $out = Join-Path $root "dist\release\$Version"
    if (Test-Path (Join-Path $out 'TEST-BUILD.txt')) { Stop-Release 'that folder holds a TEST build' }
    $msiName = "SyncWatch-$Version-x64.msi"
    $msi = Join-Path $out $msiName

    # 8. latest.json --------------------------------------------------------------------------------
    Step 'Update manifest (latest.json)'
    $manifest = [ordered]@{
        version  = $Version
        notes    = $notes
        pub_date = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
        platforms = [ordered]@{
            'windows-x86_64' = [ordered]@{
                signature = (Get-Content "$msi.sig" -Raw).Trim()
                url       = "https://github.com/$Repo/releases/download/$tag/$msiName"
                sha256    = (Get-FileHash $msi -Algorithm SHA256).Hash.ToLowerInvariant()
            }
        }
    }
    $latest = Join-Path $out 'latest.json'
    [IO.File]::WriteAllText($latest, ($manifest | ConvertTo-Json -Depth 5))
    & $pnpm --dir apps/desktop tauri signer sign -f $UpdaterKey $latest *> $null
    if (-not (Test-Path "$latest.sig")) { Stop-Release 'could not sign latest.json' }
    $sums = Get-ChildItem $out -File | Where-Object { $_.Name -ne 'SHA256SUMS' } | Sort-Object Name | ForEach-Object {
        '{0}  {1}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $_.Name
    }
    $sums | Set-Content -Encoding ascii (Join-Path $out 'SHA256SUMS')
    Ok 'latest.json and latest.json.sig written; SHA256SUMS updated'
    $assets = @($msi, "$msi.sig", $latest, "$latest.sig", (Join-Path $out 'SHA256SUMS'))
    if ($AttachPortable) { $assets += (Join-Path $out "SyncWatch-Portable-$Version-x64.zip") }

    if ($DryRun) {
        Step 'Dry run: nothing tagged, pushed or uploaded'
        $parsed = Get-Content $latest -Raw | ConvertFrom-Json
        $hashOk = $parsed.platforms.'windows-x86_64'.sha256 -eq (Get-FileHash $msi -Algorithm SHA256).Hash.ToLowerInvariant()
        if (-not $hashOk) { Stop-Release 'latest.json does not match the MSI' }
        Ok 'latest.json parses and matches the MSI'
        Write-Host 'Would upload:'
        $assets | ForEach-Object { '    {0,-40} {1,8:N1} MB' -f (Split-Path $_ -Leaf), ((Get-Item $_).Length / 1MB) }
        Write-Host "Would create tag $tag and the release https://github.com/$Repo/releases/tag/$tag"
        exit 0
    }

    # 9. tag, push, release -----------------------------------------------------------------------
    Step "Tag $tag and GitHub release"
    git tag -a $tag -m "SyncWatch $Version"
    git push origin $tag
    if ($LASTEXITCODE) { Stop-Release 'pushing the tag failed (the local tag stays; see the release guide to retry)' }
    $notesFile = Join-Path $out 'release-notes.md'
    $body = "$notes`n`n**Install:** download ``$msiName`` below and run it. Windows may show SmartScreen (the installer is not code-signed): click **More info**, then **Run anyway**. SHA-256 checksums are in ``SHA256SUMS``."
    [IO.File]::WriteAllText($notesFile, $body)
    gh release create $tag @assets --repo $Repo --title "SyncWatch $Version" --notes-file $notesFile --verify-tag
    if ($LASTEXITCODE) { Stop-Release 'creating the release failed (the tag is pushed; see the release guide)' }
    Ok 'release created'

    # 10. checks afterwards -------------------------------------------------------------------------
    Step 'Checks afterwards'
    $view = gh release view $tag --repo $Repo --json url, assets | ConvertFrom-Json
    $names = @($view.assets | ForEach-Object name)
    foreach ($a in $assets) { if ((Split-Path $a -Leaf) -notin $names) { Stop-Release "asset missing: $(Split-Path $a -Leaf)" } }
    Ok "all $($assets.Count) assets are on the release"
    $tmp = Join-Path ([IO.Path]::GetTempPath()) "sw-release-check-$PID"
    New-Item -ItemType Directory -Force $tmp | Out-Null
    try {
        Invoke-WebRequest "https://github.com/$Repo/releases/latest/download/latest.json" -OutFile (Join-Path $tmp 'latest.json') -UseBasicParsing
        $remote = Get-Content (Join-Path $tmp 'latest.json') -Raw | ConvertFrom-Json
        if ($remote.version -ne $Version) { Stop-Release "the 'latest' link still serves $($remote.version) (a newer release, or a draft?)" }
        if ($remote.platforms.'windows-x86_64'.sha256 -ne (Get-FileHash $msi -Algorithm SHA256).Hash.ToLowerInvariant()) { Stop-Release 'the published latest.json does not match the MSI' }
        $head = Invoke-WebRequest $remote.platforms.'windows-x86_64'.url -Method Head -UseBasicParsing
        Ok "latest.json is reachable, parses and matches the MSI; the MSI URL answers $($head.StatusCode)"
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
    Write-Host ''
    Write-Host "Released: $($view.url)" -ForegroundColor Green
} finally {
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
    if ($tempKeyDir) { Remove-Item -Recurse -Force $tempKeyDir -ErrorAction SilentlyContinue }
}
