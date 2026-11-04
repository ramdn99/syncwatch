#Requires -Version 7.0
<#
.SYNOPSIS
  Automated video-alignment check with REAL mouse and keyboard input (spec section 7).

.DESCRIPTION
  !!! Takes over the mouse and keyboard for about a minute. Run it only when nobody is using
  !!! the machine. It does not take screenshots.

  Starts the dev app (vite + syncwatch.exe, layout probe on, test video loaded), then:
    drags both resizers, collapses/expands both columns ([ and ]), moves and resizes the window
    by mouse, enters/leaves fullscreen (F / Esc), minimizes/restores.
  After each step it compares the mpv window's real rect (GetWindowRect on its HWND) with the
  center rect reported by the UI (1 px tolerance), checks that the video never overlaps a side
  column, and that the video window is directly under the main window. All processes it started
  are stopped at the end (port 1420 is freed).

.EXAMPLE
  pwsh -NoProfile -File scripts/dev/alignment-check.ps1 -Media D:\media\test.mkv
#>
[CmdletBinding()]
param(
    [string]$Media = 'D:\media\test.mkv',
    [int]$TolerancePx = 1
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$probe = Join-Path $env:LOCALAPPDATA 'SyncWatch\dev\layout-probe.json'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class SwInput {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [StructLayout(LayoutKind.Sequential)] struct MOUSEINPUT { public int dx, dy; public uint data, flags, time; public IntPtr extra; }
    [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort vk, scan; public uint flags, time; public IntPtr extra; }
    [StructLayout(LayoutKind.Explicit)] struct UNION { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
    [StructLayout(LayoutKind.Sequential)] struct INPUT { public uint type; public UNION u; }
    [DllImport("user32.dll")] static extern uint SendInput(uint n, INPUT[] inputs, int size);
    [DllImport("user32.dll")] static extern int GetSystemMetrics(int i);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr h, uint cmd);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern IntPtr SetProcessDpiAwarenessContext(IntPtr v);
    [StructLayout(LayoutKind.Sequential)] struct MONITORINFO { public int size; public RECT mon; public RECT work; public uint flags; }
    [DllImport("user32.dll")] static extern IntPtr MonitorFromWindow(IntPtr h, uint flags);
    [DllImport("user32.dll")] static extern bool GetMonitorInfo(IntPtr m, ref MONITORINFO mi);
    public static RECT WorkArea(IntPtr h) { var mi = new MONITORINFO(); mi.size = Marshal.SizeOf(typeof(MONITORINFO)); GetMonitorInfo(MonitorFromWindow(h, 2), ref mi); return mi.work; }
    /// Next VISIBLE window below h in z-order (skips hidden helper windows such as IME windows).
    public static IntPtr NextVisibleBelow(IntPtr h) { IntPtr n = GetWindow(h, 2); while (n != IntPtr.Zero && !IsWindowVisible(n)) n = GetWindow(n, 2); return n; }
    public static void PerMonitorAware() { SetProcessDpiAwarenessContext(new IntPtr(-4)); }
    static void Mouse(uint flags, int x, int y) {
        int vx = GetSystemMetrics(76), vy = GetSystemMetrics(77), vw = GetSystemMetrics(78), vh = GetSystemMetrics(79);
        var i = new INPUT { type = 0 };
        i.u.mi.dx = (int)Math.Round((x - vx) * 65535.0 / (vw - 1));
        i.u.mi.dy = (int)Math.Round((y - vy) * 65535.0 / (vh - 1));
        i.u.mi.flags = flags | 0x8000 | 0x4000 | 0x0001; // ABSOLUTE | VIRTUALDESK | MOVE
        SendInput(1, new[] { i }, Marshal.SizeOf(typeof(INPUT)));
    }
    public static void MoveTo(int x, int y) { Mouse(0, x, y); }
    public static void Down(int x, int y) { Mouse(0x0002, x, y); }
    public static void Up(int x, int y) { Mouse(0x0004, x, y); }
    public static void Key(ushort vk) {
        var d = new INPUT { type = 1 }; d.u.ki.vk = vk;
        var u = new INPUT { type = 1 }; u.u.ki.vk = vk; u.u.ki.flags = 2;
        SendInput(2, new[] { d, u }, Marshal.SizeOf(typeof(INPUT)));
    }
}
'@
[SwInput]::PerMonitorAware()

$VK = @{ LBracket = 0xDB; RBracket = 0xDD; F = 0x46; Esc = 0x1B }
$results = [System.Collections.Generic.List[pscustomobject]]::new()
$started = @()
$script:firstMain = $null

function Read-Probe { Get-Content $probe -Raw | ConvertFrom-Json }
function Rect-Of([IntPtr]$h) { $r = New-Object SwInput+RECT; [void][SwInput]::GetWindowRect($h, [ref]$r); $r }

function Drag([int]$x1, [int]$y1, [int]$x2, [int]$y2) {
    [SwInput]::MoveTo($x1, $y1); Start-Sleep -Milliseconds 120
    [SwInput]::Down($x1, $y1); Start-Sleep -Milliseconds 120
    $steps = 12
    for ($i = 1; $i -le $steps; $i++) {
        [SwInput]::MoveTo([int]($x1 + ($x2 - $x1) * $i / $steps), [int]($y1 + ($y2 - $y1) * $i / $steps)); Start-Sleep -Milliseconds 25
    }
    [SwInput]::Up($x2, $y2); Start-Sleep -Milliseconds 150
}

function Check([string]$step, [switch]$ExpectHidden) {
    Start-Sleep -Milliseconds 700
    $p = Read-Probe
    $main = [IntPtr][long]$p.mainHwnd; $video = [IntPtr][long]$p.videoHwnd
    $ok = $true; $why = @()
    if ($script:firstMain -and $p.mainHwnd -ne $script:firstMain) { $ok = $false; $why += 'app restarted during the test (window handle changed)' }
    if ($ExpectHidden) {
        if ([SwInput]::IsWindowVisible($video)) { $ok = $false; $why += 'video visible while main minimized' }
        if (-not [SwInput]::IsIconic($main)) { $ok = $false; $why += 'main not minimized' }
    } else {
        if (-not $p.expected) { $ok = $false; $why += "no expected rect (inputs: $($p.inputs | ConvertTo-Json -Compress))" }
        else {
            $a = Rect-Of $video
            $e = $p.expected
            $d = @([math]::Abs($a.L - $e.x), [math]::Abs($a.T - $e.y), [math]::Abs(($a.R - $a.L) - $e.w), [math]::Abs(($a.B - $a.T) - $e.h)) | Measure-Object -Maximum
            if ($d.Maximum -gt $TolerancePx) { $ok = $false; $why += "off by $($d.Maximum) px" }
            foreach ($c in @($p.columns)) {
                if ($a.L -lt $c.x + $c.w -and $c.x -lt $a.R -and $a.T -lt $c.y + $c.h -and $c.y -lt $a.B) { $ok = $false; $why += 'overlaps a side column' }
            }
            if ([SwInput]::NextVisibleBelow($main) -ne $video) { $ok = $false; $why += 'video is not directly under main' }
        }
    }
    $results.Add([pscustomobject]@{ Step = $step; Result = $(if ($ok) { 'PASS' } else { 'FAIL' }); Detail = ($why -join '; ') })
    Write-Host ('{0,-4} {1} {2}' -f $(if ($ok) { 'PASS' } else { 'FAIL' }), $step, ($why -join '; ')) -ForegroundColor $(if ($ok) { 'Green' } else { 'Red' })
}

try {
    if (Get-NetTCPConnection -LocalPort 1420 -State Listen -ErrorAction SilentlyContinue) {
        throw 'Port 1420 is busy (another dev server?). Close it first.'
    }
    Remove-Item $probe -ErrorAction SilentlyContinue
    $env:SYNCWATCH_DEV_LAYOUT_PROBE = '1'
    $env:SYNCWATCH_DEV_MEDIA = $Media
    # No `tauri dev`: its file watcher can relaunch the app mid-test. Use the prebuilt debug exe
    # plus a plain Vite dev server instead (build first: cargo build -p syncwatch-desktop).
    $exe = Join-Path $root 'target/debug/syncwatch.exe'
    if (-not (Test-Path $exe)) { throw "Build first: cargo build -p syncwatch-desktop ($exe missing)" }
    $pnpm = (Get-Command pnpm.cmd -ErrorAction SilentlyContinue)?.Source ?? "$env:APPDATA\npm\pnpm.cmd"
    $started += Start-Process $pnpm -ArgumentList '--dir', (Join-Path $root 'apps/desktop'), 'dev' -PassThru -WindowStyle Minimized
    $viteDeadline = (Get-Date).AddMinutes(1)
    while (-not (Get-NetTCPConnection -LocalPort 1420 -State Listen -ErrorAction SilentlyContinue)) {
        if ((Get-Date) -gt $viteDeadline) { throw 'Vite dev server did not start on port 1420.' }
        Start-Sleep -Milliseconds 300
    }
    $started += Start-Process $exe -PassThru
    $deadline = (Get-Date).AddMinutes(5)
    while (-not (Test-Path $probe) -or -not (Read-Probe).expected) {
        if ((Get-Date) -gt $deadline) { throw 'App did not report a layout within 5 minutes.' }
        Start-Sleep -Seconds 1
    }
    Start-Sleep -Seconds 2
    $p = Read-Probe
    $main = [IntPtr][long]$p.mainHwnd
    $script:firstMain = $p.mainHwnd
    [void][SwInput]::SetForegroundWindow($main)
    Check 'initial layout'

    # Click into the video area (no-op click) so shortcuts are not typed into the chat box.
    $e = (Read-Probe).expected
    [SwInput]::MoveTo($e.x + [int]($e.w / 2), $e.y + 40); [SwInput]::Down($e.x + [int]($e.w / 2), $e.y + 40); [SwInput]::Up($e.x + [int]($e.w / 2), $e.y + 40)

    $cols = @((Read-Probe).columns)
    $left = $cols | Sort-Object x | Select-Object -First 1
    $right = $cols | Sort-Object x | Select-Object -Last 1
    $lx = $left.x + $left.w + 2; $ly = $left.y + [int]($left.h / 2)
    Drag $lx $ly ($lx + 150) $ly; Check 'drag left resizer +150'
    Drag ($lx + 150) $ly ($lx - 60) $ly; Check 'drag left resizer -210'
    $cols = @((Read-Probe).columns); $right = $cols | Sort-Object x | Select-Object -Last 1
    $rx = $right.x - 2; $ry = $right.y + [int]($right.h / 2)
    Drag $rx $ry ($rx - 120) $ry; Check 'drag right resizer -120'
    Drag ($rx - 120) $ry ($rx + 80) $ry; Check 'drag right resizer +200'

    [SwInput]::Key($VK.LBracket); Check 'collapse chat ([)'
    [SwInput]::Key($VK.RBracket); Check 'collapse right column (])'
    [SwInput]::Key($VK.LBracket); Check 'expand chat ([)'
    [SwInput]::Key($VK.RBracket); Check 'expand right column (])'

    $w = Rect-Of $main
    $tx = $w.L + 200; $ty = $w.T + 12
    Drag $tx $ty ($tx + 140) ($ty + 90); Check 'move window by title bar'
    # Corner drags must start and end inside the monitor work area: a mouse-down on the taskbar
    # would hit the app's own taskbar button and minimize it.
    function Fit-ForCornerDrag([int]$growX, [int]$growY) {
        $w = Rect-Of $main; $wa = [SwInput]::WorkArea($main)
        $dx = [math]::Min(0, ($wa.R - 20) - ($w.R + $growX)); $dy = [math]::Min(0, ($wa.B - 20) - ($w.B + $growY))
        if ($dx -lt 0 -or $dy -lt 0) {
            $tx = $w.L + 200; $ty = $w.T + 12
            Drag $tx $ty ($tx + $dx) ($ty + $dy)
        }
    }
    Fit-ForCornerDrag 0 0
    $w = Rect-Of $main
    Drag ($w.R - 3) ($w.B - 3) ($w.R - 180) ($w.B - 120); Check 'resize window from corner (smaller)'
    Fit-ForCornerDrag 120 80
    $w = Rect-Of $main
    Drag ($w.R - 3) ($w.B - 3) ($w.R + 120) ($w.B + 80); Check 'resize window from corner (larger)'

    [SwInput]::Key($VK.F); Start-Sleep -Milliseconds 800; Check 'enter fullscreen (F)'
    [SwInput]::Key($VK.Esc); Start-Sleep -Milliseconds 800; Check 'leave fullscreen (Esc)'

    [void][SwInput]::ShowWindow($main, 6); Check 'minimize' -ExpectHidden
    [void][SwInput]::ShowWindow($main, 9); [void][SwInput]::SetForegroundWindow($main); Check 'restore'
}
finally {
    Get-Process syncwatch -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Seconds 2
    foreach ($s in $started) { if (-not $s.HasExited) { & taskkill /PID $s.Id /T /F *> $null } }
    $busy = Get-NetTCPConnection -LocalPort 1420 -State Listen -ErrorAction SilentlyContinue | Where-Object OwningProcess -ne 0
    if ($busy) { Write-Host "WARN port 1420 still held by PID $($busy.OwningProcess)" -ForegroundColor Yellow }
}

$fails = @($results | Where-Object Result -eq 'FAIL').Count
Write-Host ''
Write-Host "alignment-check: $($results.Count - $fails) PASS, $fails FAIL"
if ($fails -or $results.Count -eq 0) { exit 1 }
exit 0
