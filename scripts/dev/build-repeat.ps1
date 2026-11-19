#Requires -Version 7.0
<#
.SYNOPSIS
  Build stability check: N standalone release builds in a row, each from a CLEAN target folder.

.DESCRIPTION
  Uses a separate target folder (CARGO_TARGET_DIR, default under %TEMP%) so the normal
  target\ is not touched. For each run: delete that folder, run
  `pnpm --dir apps/desktop tauri build --no-bundle`, and check that syncwatch.exe,
  libmpv-2.dll, vulkan\vulkan-1.dll and the four NotoSans fonts are there and identical to
  their sources. Prints one line per run and a summary; deletes the folder at the end.
  Exit code 1 if any run failed. The full output of a failed build is saved to
  %TEMP%\sw-build-fail-<time>-run<N>.log and its path printed.

.EXAMPLE
  pwsh -NoProfile -File scripts/dev/build-repeat.ps1 -Count 10
#>
param([int]$Count = 10, [string]$TargetDir = (Join-Path ([IO.Path]::GetTempPath()) 'sw-build-repeat'))
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
Set-Location $repo
if (-not $env:RUSTUP_HOME) { $env:RUSTUP_HOME = 'D:\Rust\rustup' }
if (-not $env:CARGO_HOME) { $env:CARGO_HOME = 'D:\Rust\cargo' }
if (-not $env:LIBMPV_DIR) { $env:LIBMPV_DIR = 'D:\libs\mpv' }
$env:PATH = "$env:CARGO_HOME\bin;$env:PATH"
$env:CARGO_TARGET_DIR = $TargetDir
$pnpm = if (Get-Command pnpm.cmd -ErrorAction SilentlyContinue) { 'pnpm.cmd' } else { 'pnpm' }

$expect = @(
    @{ Out = 'libmpv-2.dll'; Src = Join-Path $env:LIBMPV_DIR 'libmpv-2.dll' },
    @{ Out = 'vulkan\vulkan-1.dll'; Src = 'third_party\cache\vulkan\vulkan-1.dll' }
) + @('Regular', 'Bold', 'Italic', 'BoldItalic' | ForEach-Object { @{ Out = "fonts\NotoSans-$_.ttf"; Src = "third_party\cache\fonts\NotoSans-$_.ttf" } })

$failed = 0
for ($i = 1; $i -le $Count; $i++) {
    if (Test-Path $TargetDir) { Remove-Item -Recurse -Force $TargetDir }
    $t = Get-Date
    $log = & $pnpm --dir apps/desktop tauri build --no-bundle 2>&1 | Out-String
    $code = $LASTEXITCODE
    $secs = [int]((Get-Date) - $t).TotalSeconds
    $rel = Join-Path $TargetDir 'release'
    $problems = @()
    if ($code -ne 0) { $problems += "exit $code" }
    if (-not (Test-Path (Join-Path $rel 'syncwatch.exe'))) { $problems += 'no syncwatch.exe' }
    foreach ($e in $expect) {
        $out = Join-Path $rel $e.Out
        if (-not (Test-Path $out)) { $problems += "missing $($e.Out)"; continue }
        if ((Get-FileHash $out).Hash -ne (Get-FileHash $e.Src).Hash) { $problems += "differs $($e.Out)" }
    }
    $warn = @($log -split "`n" | Where-Object { $_ -match 'warning: syncwatch-desktop@' }).Count
    if ($problems) {
        $failed++
        Write-Host ("run {0,2}: FAIL in {1} s: {2}" -f $i, $secs, ($problems -join ', ')) -ForegroundColor Red
        # The full output of every failed build is kept (the cause of a one-off failure must never be lost).
        $keep = Join-Path ([IO.Path]::GetTempPath()) ("sw-build-fail-{0:yyyyMMdd-HHmmss}-run{1}.log" -f (Get-Date), $i)
        $log | Set-Content -Encoding utf8 $keep
        Write-Host "    full log: $keep"
        $log -split "`n" | Where-Object { $_ -match 'error|failed|warning: syncwatch-desktop' } | Select-Object -First 15 | ForEach-Object { "    $_" }
    } else {
        Write-Host ("run {0,2}: PASS in {1} s (build-script warnings: {2})" -f $i, $secs, $warn)
    }
}
if (Test-Path $TargetDir) { Remove-Item -Recurse -Force $TargetDir }
Write-Host ("{0} of {1} clean builds passed" -f ($Count - $failed), $Count)
if ($failed) { exit 1 }
