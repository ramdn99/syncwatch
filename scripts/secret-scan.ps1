#Requires -Version 7.0
<#
.SYNOPSIS
  SyncWatch secret scan. Never prints secret values.

.DESCRIPTION
  Two layers:
    1. Value scan: loads the REAL secret values from the git-ignored .env.local
       and .env.ice.local and looks for them (raw, URL-encoded, lower-case) in
       the selected scope. Only file names / revisions are reported, never the
       matching line.
    2. gitleaks (pinned Docker image, --redact) for generic secret patterns.

  Modes:
    Staged   - staged blobs (pre-commit hook)
    Tree     - tracked + untracked-but-not-ignored files in the working tree
    History  - every commit reachable from any ref (pre-push hook, before you push)
    Paths    - arbitrary files/folders given with -Path (e.g. log folders)
    All      - Tree + History

.EXAMPLE
  pwsh -NoProfile -File scripts/secret-scan.ps1 -Mode History
#>
[CmdletBinding()]
param(
    [ValidateSet('Staged', 'Tree', 'History', 'Paths', 'All')]
    [string]$Mode = 'All',
    [string[]]$Path = @(),
    [switch]$SkipGitleaks
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Pinned image: tag + digest (see third_party/pins.json "gitleaks").
$GitleaksImage = 'zricethezav/gitleaks:v8.28.0@sha256:cdbb7c955abce02001a9f6c9f602fb195b7fadc1e812065883f695d1eeaba854'
$MinSecretLength = 6

$repo = (git -C $PSScriptRoot rev-parse --show-toplevel).Trim()

function Get-SecretValues {
    param([string]$Root)
    $values = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $envFile = Join-Path $Root '.env.local'
    if (Test-Path $envFile) {
        foreach ($line in Get-Content $envFile) {
            if ($line -match '^\s*#' -or $line -notmatch '=') { continue }
            $v = ($line -split '=', 2)[1].Trim().Trim('"').Trim("'")
            if ($v) { [void]$values.Add($v) }
        }
    }
    $iceFile = Join-Path $Root '.env.ice.local'
    if (Test-Path $iceFile) {
        $text = Get-Content $iceFile -Raw
        $rx = '(?i)["'']?(username|credential)["'']?\s*:\s*["'']([^"'']+)["'']'
        foreach ($m in [regex]::Matches($text, $rx)) { [void]$values.Add($m.Groups[2].Value) }
    }
    $out = [System.Collections.Generic.List[string]]::new()
    foreach ($v in $values) {
        if ($v.Length -lt $MinSecretLength) { continue }
        $out.Add($v)
        $enc = [uri]::EscapeDataString($v)
        if ($enc -ne $v) { $out.Add($enc) }
        $low = $v.ToLowerInvariant()
        if ($low -ne $v) { $out.Add($low) }
    }
    return , $out
}

$secrets = Get-SecretValues -Root $repo
$failed = $false
$patternFile = $null

try {
    if ($secrets.Count -eq 0) {
        Write-Host 'WARN  value scan: no secret values found in .env.local / .env.ice.local (nothing to look for).'
    } else {
        # Pattern file lives in the user temp dir, outside the repo, and is deleted in finally.
        $patternFile = [System.IO.Path]::GetTempFileName()
        [System.IO.File]::WriteAllLines($patternFile, $secrets)
        Write-Host "INFO  value scan: checking $($secrets.Count) secret variants (values not shown)."

        $hits = [System.Collections.Generic.List[string]]::new()
        $doTree = $Mode -in 'Tree', 'All'
        $doHistory = $Mode -in 'History', 'All'

        if ($Mode -eq 'Staged') {
            $r = git -C $repo grep --cached -l -I -F -f $patternFile 2>$null
            if ($r) { $r | ForEach-Object { $hits.Add("staged:$_") } }
        }
        if ($doTree) {
            $r = git -C $repo grep --untracked -l -I -F -f $patternFile 2>$null
            if ($r) { $r | ForEach-Object { $hits.Add("tree:$_") } }
        }
        if ($doHistory) {
            $revs = @(git -C $repo rev-list --all 2>$null)
            foreach ($rev in $revs) {
                $r = git -C $repo grep -l -I -F -f $patternFile $rev 2>$null
                if ($r) { $r | ForEach-Object { $hits.Add("commit:$_") } }
                $msg = git -C $repo log -1 --format=%B $rev
                foreach ($s in $secrets) { if ($msg -and $msg.Contains($s)) { $hits.Add("commit-message:$($rev.Substring(0,12))") ; break } }
            }
            Write-Host "INFO  value scan: $($revs.Count) commit(s) checked."
        }
        if ($Mode -eq 'Paths') {
            foreach ($p in $Path) {
                $files = if (Test-Path $p -PathType Container) { Get-ChildItem $p -Recurse -File } else { Get-Item $p }
                foreach ($f in $files) {
                    $content = [System.IO.File]::ReadAllText($f.FullName)
                    foreach ($s in $secrets) { if ($content.Contains($s)) { $hits.Add("file:$($f.FullName)"); break } }
                }
            }
        }

        if ($hits.Count -gt 0) {
            $failed = $true
            Write-Host 'FAIL  value scan: a real secret value was found in:' -ForegroundColor Red
            $hits | Sort-Object -Unique | ForEach-Object { Write-Host "        $_" -ForegroundColor Red }
        } else {
            Write-Host 'PASS  value scan: no secret values found.' -ForegroundColor Green
        }
    }

    if (-not $SkipGitleaks -and $Mode -ne 'Paths') {
        $docker = Get-Command docker -ErrorAction SilentlyContinue
        if (-not $docker) {
            $known = 'C:\Users\ramdn\AppData\Local\Programs\DockerDesktop\resources\bin\docker.exe'
            if (Test-Path $known) { $docker = Get-Item $known }
        }
        $dockerOk = $false
        if ($docker) { & $docker.Source info *> $null; $dockerOk = ($LASTEXITCODE -eq 0) }
        if (-not $dockerOk) {
            Write-Host 'FAIL  gitleaks: Docker is not running. Start Docker Desktop, or re-run with -SkipGitleaks (value scan only).' -ForegroundColor Red
            $failed = $true
        } else {
            $hasCommits = [bool](git -C $repo rev-list -n 1 --all 2>$null)
            $common = @('run', '--rm', '-v', "${repo}:/repo:ro",
                '-e', 'GIT_CONFIG_COUNT=1', '-e', 'GIT_CONFIG_KEY_0=safe.directory', '-e', 'GIT_CONFIG_VALUE_0=*',
                $GitleaksImage)
            $runs = @()
            if ($Mode -eq 'Staged') { $runs += , @('git', '--pre-commit', '--staged', '--redact', '--no-banner', '/repo') }
            if ($Mode -in 'History', 'All' -and $hasCommits) { $runs += , @('git', '--redact', '--no-banner', '/repo') }
            if ($Mode -eq 'Tree') { Write-Host 'INFO  gitleaks: Tree mode uses the value scan only (gitleaks dir would read the ignored env files).' }
            foreach ($args2 in $runs) {
                & $docker.Source @common @args2
                if ($LASTEXITCODE -ne 0) { $failed = $true; Write-Host "FAIL  gitleaks ($($args2[0..2] -join ' '))" -ForegroundColor Red }
                else { Write-Host "PASS  gitleaks ($($args2[0..2] -join ' '))" -ForegroundColor Green }
            }
        }
    }
} finally {
    if ($patternFile -and (Test-Path $patternFile)) { Remove-Item $patternFile -Force }
}

if ($failed) { exit 1 }
exit 0
