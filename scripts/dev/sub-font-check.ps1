#Requires -Version 7.0
<#
.SYNOPSIS
  Subtitle font check (Milestone 8, B8): non-Latin subtitles render without missing-glyph boxes.

.DESCRIPTION
  Renders scripts\dev\fixtures\subtitle-scripts.srt (Latin, Greek, Cyrillic, Arabic, Hebrew,
  Chinese, Japanese, Korean, Thai, Hindi) over a short clip with the app's own subtitle font
  setup (bundled Noto Sans, then the system fonts), using the sub_font_check example. mpv's
  vo=image draws the subtitles into PNG frames: no window, no screen capture, no input.
  Two runs:
    app         what the app does; every line must show real letters;
    nofallback  the control, system fonts off: only Latin, Greek and Cyrillic may render, the
                rest must be boxes (proves the check can see boxes).
  Writes sheet-app.png and sheet-nofallback.png (one frame per subtitle line) and prints their
  paths: open them and compare. Nothing keeps running.

  Clean VM (Milestone 9): copy sub_font_check.exe, libmpv-2.dll, the fonts folder, this script,
  the fixture and a short video to the VM and run with -Exe, -Fonts, -Video (no ffmpeg or Rust
  needed there). Without ffmpeg no sheet is made: open the PNG frames in the two folders.

.EXAMPLE
  pwsh -NoProfile -File scripts/dev/sub-font-check.ps1
.EXAMPLE
  pwsh -NoProfile -File sub-font-check.ps1 -Exe .\sub_font_check.exe -Fonts .\fonts -Video .\clip.mp4 -Out .\out
#>
param(
    [string]$Exe,
    [string]$Fonts,
    [string]$Video,
    [string]$Out = (Join-Path ([IO.Path]::GetTempPath()) "sw-subfont-$PID")
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$srt = Join-Path $PSScriptRoot 'fixtures\subtitle-scripts.srt'
if (-not (Test-Path $srt)) { $srt = Join-Path $PSScriptRoot 'subtitle-scripts.srt' }
New-Item -ItemType Directory -Force $Out | Out-Null
$ffmpeg = Get-Command ffmpeg -ErrorAction SilentlyContinue

if (-not $Exe) {
    foreach ($k in @{ RUSTUP_HOME = 'D:\Rust\rustup'; CARGO_HOME = 'D:\Rust\cargo'; LIBMPV_DIR = 'D:\libs\mpv' }.GetEnumerator()) {
        if (-not [Environment]::GetEnvironmentVariable($k.Key)) { Set-Item "Env:$($k.Key)" $k.Value }
    }
    $env:PATH = "$env:CARGO_HOME\bin;$env:LIBMPV_DIR;$env:PATH"
    Push-Location $repo
    try { cargo build -q -p syncwatch-player --example sub_font_check; if ($LASTEXITCODE) { throw 'build failed' } } finally { Pop-Location }
    $Exe = Join-Path $repo 'target\debug\examples\sub_font_check.exe'
}
if (-not $Fonts) { $Fonts = Join-Path $repo 'third_party\cache\fonts' }
if (-not (Test-Path (Join-Path $Fonts 'NotoSans-Regular.ttf'))) { throw "NotoSans-Regular.ttf not found in $Fonts (run scripts/fetch-third-party.ps1)" }
if (-not $Video) {
    if (-not $ffmpeg) { throw 'Pass -Video (any short video): ffmpeg is not available to make one.' }
    $Video = Join-Path $Out 'clip.mp4'
    & ffmpeg -hide_banner -loglevel error -y -f lavfi -i 'color=c=0x303040:s=960x540:r=2:d=8' -c:v libx264 -pix_fmt yuv420p $Video
}

foreach ($m in 'app', 'nofallback') {
    $dir = Join-Path $Out $m
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
    & $Exe $Video $srt $Fonts $dir $m
    if ($LASTEXITCODE) { throw "$m run failed" }
}
if ($ffmpeg) {
    foreach ($m in 'app', 'nofallback') {
        # One frame per subtitle line (2 fps clip, 1 s per line), the bottom band only.
        & ffmpeg -hide_banner -loglevel error -y -i (Join-Path $Out "$m\%08d.png") -vf "select='not(mod(n\,2))',crop=iw:ih*0.36:0:ih*0.64,tile=2x4" -frames:v 1 (Join-Path $Out "sheet-$m.png")
    }
    Write-Host "Open and compare:"
    Write-Host "  $(Join-Path $Out 'sheet-app.png')         (every line real letters)"
    Write-Host "  $(Join-Path $Out 'sheet-nofallback.png')  (control: non-Latin lines are boxes)"
} else {
    Write-Host "Frames (no ffmpeg for a sheet): $(Join-Path $Out 'app') and $(Join-Path $Out 'nofallback')"
}
