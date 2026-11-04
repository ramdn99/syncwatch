#Requires -Version 7.0
<#
.SYNOPSIS
  Captures ONLY the SyncWatch windows by handle (PrintWindow), never the screen.

.DESCRIPTION
  Reads the window handles from the dev layout probe
  (%LOCALAPPDATA%\SyncWatch\dev\layout-probe.json, written when SYNCWATCH_DEV_LAYOUT_PROBE=1)
  and writes three PNGs to -OutDir:
    main.png      - the main (WebView2) window as rendered by PrintWindow
    video.png     - the mpv video window
    composite.png - main window with the video window's pixels placed in the center column
  Anything else on screen (other apps, desktop) is never read.

.EXAMPLE
  pwsh -NoProfile -File scripts/dev/capture-window.ps1 -OutDir $env:TEMP\syncwatch-capture
#>
[CmdletBinding()]
param([Parameter(Mandatory)][string]$OutDir)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class SwCapture {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] static extern IntPtr SetProcessDpiAwarenessContext(IntPtr v);
    public static void PerMonitorAware() { SetProcessDpiAwarenessContext(new IntPtr(-4)); }
}
"@

# Captures one window by handle (PW_RENDERFULLCONTENT); never reads the screen.
function Capture-Window([IntPtr]$h) {
    $r = New-Object SwCapture+RECT
    [void][SwCapture]::GetWindowRect($h, [ref]$r)
    $bmp = New-Object System.Drawing.Bitmap ([math]::Max(1, $r.R - $r.L)), ([math]::Max(1, $r.B - $r.T))
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    [void][SwCapture]::PrintWindow($h, $hdc, 2)
    $g.ReleaseHdc($hdc)
    $g.Dispose()
    return $bmp
}
[SwCapture]::PerMonitorAware()

$probe = Join-Path $env:LOCALAPPDATA 'SyncWatch\dev\layout-probe.json'
$p = Get-Content $probe -Raw | ConvertFrom-Json
$main = [IntPtr][long]$p.mainHwnd
$video = [IntPtr][long]$p.videoHwnd
New-Item -ItemType Directory -Force $OutDir | Out-Null

$m = Capture-Window $main
$v = Capture-Window $video
$m.Save((Join-Path $OutDir 'main.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$v.Save((Join-Path $OutDir 'video.png'), [System.Drawing.Imaging.ImageFormat]::Png)

$mr = New-Object SwCapture+RECT; [void][SwCapture]::GetWindowRect($main, [ref]$mr)
$vr = New-Object SwCapture+RECT; [void][SwCapture]::GetWindowRect($video, [ref]$vr)
$c = New-Object System.Drawing.Bitmap $m.Width, $m.Height
$g = [System.Drawing.Graphics]::FromImage($c)
$g.DrawImage($m, 0, 0)
$g.DrawImage($v, $vr.L - $mr.L, $vr.T - $mr.T)
$g.Dispose()
$c.Save((Join-Path $OutDir 'composite.png'), [System.Drawing.Imaging.ImageFormat]::Png)
Write-Host "captured main ($($m.Width)x$($m.Height)) and video ($($v.Width)x$($v.Height)) to $OutDir"
