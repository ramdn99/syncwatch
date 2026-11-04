#Requires -Version 7.0
# Self-test for verify-imports.ps1 (positive AND negative cases). No Pester needed.
#   pwsh -NoProfile -File scripts/tests/verify-imports.tests.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$verify = Join-Path $root 'scripts/verify-imports.ps1'
$libmpv = Join-Path $root 'target/debug/libmpv-2.dll'
$exe = Join-Path $root 'target/debug/syncwatch.exe'
$failures = 0

function Expect([string]$name, [int]$expectedExit, [string[]]$args2, [string]$mustContain) {
    $out = & pwsh -NoProfile -File $verify @args2 2>&1 | Out-String
    $code = $LASTEXITCODE
    $ok = ($code -eq $expectedExit) -and (-not $mustContain -or $out -match [regex]::Escape($mustContain))
    if ($ok) { Write-Host "ok    $name" -ForegroundColor Green }
    else { Write-Host "FAIL  $name (exit $code, expected $expectedExit)`n$out" -ForegroundColor Red; $script:failures++ }
}

Expect 'libmpv passes with the vulkan exception' 0 @('-Path', $libmpv) 'PASS'
Expect 'syncwatch.exe passes (static CRT, delay-loaded libmpv)' 0 @('-Path', $exe) 'delay-load: libmpv-2.dll'
Expect 'NEGATIVE: libmpv fails without the vulkan exception' 1 @('-Path', $libmpv, '-NoVulkanException') 'vulkan-1.dll'
Expect 'NEGATIVE: a denied system DLL is reported' 1 @('-Path', $exe, '-Deny', 'kernel32.dll') 'kernel32.dll'
Expect 'NEGATIVE: a missing file fails' 1 @('-Path', (Join-Path $root 'target/debug/does-not-exist.dll')) 'not found'

if ($failures) { exit 1 }
Write-Host 'verify-imports self-test passed.' -ForegroundColor Green
