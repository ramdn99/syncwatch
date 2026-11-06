#Requires -Version 7.0
<#
.SYNOPSIS
  Creates the Milestone 6 test media. Nothing goes into the repo, and the original
  D:\media\test.mkv is only read, never written.

  - <OutDir>\long.mkv: 63-minute generated video (2 audio tracks eng/jpn, 1 English subtitle)
    for the 1:02:03 chat-chip test.
  - <OutDir>\extra-cp1252.srt: external subtitle in Windows-1252 (tests the UTF-8 conversion).
  - <OutDir>\extra-audio.mka: external audio file.
  - D:\media\test-mismatch.mkv: a COPY of test.mkv with a different title tag, so its identity
    differs (the "file differs" flow). Made by copying first, then editing only the copy.

  Needs ffmpeg and MKVToolNix (mkvpropedit). Idempotent: existing files are kept.

.EXAMPLE
  pwsh -NoProfile -File scripts/dev/make-test-media.ps1
#>
[CmdletBinding()]
param(
    [string]$OutDir = (Join-Path ([IO.Path]::GetTempPath()) 'syncwatch-media'),
    [string]$Original = 'D:\media\test.mkv',
    [string]$Mismatch = 'D:\media\test-mismatch.mkv'
)
if ($PSVersionTable.PSVersion.Major -lt 7) { Write-Host 'Run with PowerShell 7: pwsh -NoProfile -File scripts/dev/make-test-media.ps1'; exit 2 }
$ErrorActionPreference = 'Stop'
$ffmpeg = (Get-Command ffmpeg -ErrorAction SilentlyContinue)?.Source
if (-not $ffmpeg) { throw 'ffmpeg not found (winget install ffmpeg).' }
New-Item -ItemType Directory -Force $OutDir | Out-Null

$long = Join-Path $OutDir 'long.mkv'
if (-not (Test-Path $long)) {
    $srt = Join-Path $OutDir 'long.srt'
    $lines = for ($i = 0; $i -lt 400; $i++) {
        $s = $i * 9 + 1
        "{0}`r`n{1:hh\:mm\:ss\,fff} --> {2:hh\:mm\:ss\,fff}`r`nLine {0} at {1:hh\:mm\:ss}`r`n" -f ($i + 1), [TimeSpan]::FromSeconds($s), [TimeSpan]::FromSeconds($s + 4)
    }
    [IO.File]::WriteAllText($srt, ($lines -join "`r`n"))
    Write-Host 'Encoding long.mkv (63 min, small)...'
    & $ffmpeg -hide_banner -loglevel error -y -f lavfi -i 'testsrc2=size=320x180:rate=5:duration=3800' -f lavfi -i 'sine=frequency=440:duration=3800' -f lavfi -i 'sine=frequency=660:duration=3800' -i $srt `
        -map 0:v -map 1:a -map 2:a -map 3:s -c:v libx264 -preset ultrafast -tune stillimage -crf 35 -g 50 -c:a aac -b:a 32k -c:s srt `
        -metadata:s:a:0 language=eng -metadata:s:a:0 'title=Tone A' -metadata:s:a:1 language=jpn -metadata:s:a:1 'title=Tone B' -metadata:s:s:0 language=eng $long
    if ($LASTEXITCODE) { throw 'ffmpeg failed (long.mkv)' }
}

$sub = Join-Path $OutDir 'extra-cp1252.srt'
if (-not (Test-Path $sub)) {
    $text = "1`r`n00:00:01,000 --> 00:00:30,000`r`nExterne Untertitel: caf$([char]0xE9) cr$([char]0xE8)me br$([char]0xFB)l$([char]0xE9)e`r`n`r`n2`r`n00:00:31,000 --> 00:01:00,000`r`nZweite Zeile $([char]0xE0) la carte`r`n"
    [IO.File]::WriteAllBytes($sub, [Text.Encoding]::GetEncoding(1252).GetBytes($text))
}

$audio = Join-Path $OutDir 'extra-audio.mka'
if (-not (Test-Path $audio)) {
    & $ffmpeg -hide_banner -loglevel error -y -f lavfi -i 'sine=frequency=330:duration=3800' -c:a libopus -b:a 16k $audio
    if ($LASTEXITCODE) { throw 'ffmpeg failed (extra-audio.mka)' }
}

if ((Test-Path $Original) -and -not (Test-Path $Mismatch)) {
    $mkvpe = Get-ChildItem 'D:\Program Files\MKVToolNix', 'C:\Program Files\MKVToolNix' -Filter mkvpropedit.exe -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName
    if (-not $mkvpe) { throw 'mkvpropedit (MKVToolNix) not found.' }
    $before = (Get-FileHash $Original -Algorithm SHA256).Hash
    Copy-Item $Original $Mismatch
    & $mkvpe $Mismatch --edit info --set 'title=SyncWatch mismatch test copy' | Out-Null
    if ((Get-FileHash $Original -Algorithm SHA256).Hash -ne $before) { throw 'The original changed: stop and investigate.' }
}
Get-ChildItem $OutDir | Select-Object Name, Length | Format-Table
if (Test-Path $Mismatch) { Write-Host "Mismatch copy: $Mismatch" }
