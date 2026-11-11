#Requires -Version 7
<#
.SYNOPSIS
  Fails if the Windows dependency tree contains aws-lc, native-tls or OpenSSL (ADRs 0004, 0009).
.DESCRIPTION
  Runs cargo tree for x86_64-pc-windows-msvc with every inverted crate. OpenSSL is listed in
  Cargo.lock for Linux only (crypto-hash); it is never built for Windows (docs/build-guide.md).
#>
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
$banned = 'aws-lc-rs', 'aws-lc-sys', 'native-tls', 'openssl', 'openssl-sys'
$fail = $false
foreach ($c in $banned) {
    $out = & cargo tree --workspace --locked --target x86_64-pc-windows-msvc -e normal,build -i $c 2>&1 | Out-String
    # "did not match any packages" (not in the lock at all) or "nothing to print" (in the lock,
    # but not for this target) both mean: not in the Windows build.
    if ($out -match 'did not match any packages|nothing to print') {
        Write-Host "PASS  $c not in the Windows build"
    } else {
        Write-Host "FAIL  $c is in the Windows build:" -ForegroundColor Red
        Write-Host $out
        $fail = $true
    }
}
$ring = & cargo tree --workspace --locked --target x86_64-pc-windows-msvc -e normal -i ring 2>&1 | Select-Object -First 1
Write-Host "INFO  crypto: $ring"
if ($fail) { exit 1 }
