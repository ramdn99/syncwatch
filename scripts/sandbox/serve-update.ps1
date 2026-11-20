<#
.SYNOPSIS
  A tiny local update server for the Windows Sandbox test (Windows PowerShell 5.1 or 7).
  Serves the files of one folder on http://127.0.0.1:<Port>/ with GET and HEAD, one request at
  a time. Uses a TcpListener, so it needs no URL reservation or administrator rights. Loopback
  only; test use only.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File serve-update.ps1 -Dir C:\SyncWatchTest\update -Port 8765
#>
param(
    [Parameter(Mandatory = $true)][string]$Dir,
    [int]$Port = 8765,
    [int]$Minutes = 30
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path $Dir).Path
$listener = New-Object System.Net.Sockets.TcpListener ([System.Net.IPAddress]::Loopback, $Port)
$listener.Start()
Write-Host "Serving $root on http://127.0.0.1:$Port/ for $Minutes minutes"
$end = (Get-Date).AddMinutes($Minutes)
try {
    while ((Get-Date) -lt $end) {
        if (-not $listener.Pending()) { Start-Sleep -Milliseconds 100; continue }
        $client = $listener.AcceptTcpClient()
        try {
            $stream = $client.GetStream()
            $reader = New-Object System.IO.StreamReader($stream, [System.Text.Encoding]::ASCII, $false, 4096, $true)
            $line = $reader.ReadLine()
            while ($true) { $h = $reader.ReadLine(); if ([string]::IsNullOrEmpty($h)) { break } }
            $parts = "$line".Split(' ')
            $method = $parts[0]
            $name = [Uri]::UnescapeDataString(("$($parts[1])".TrimStart('/')).Split('?')[0])
            $path = Join-Path $root $name
            $ok = ($method -eq 'GET' -or $method -eq 'HEAD') -and $name -and ($name -notmatch '[\\/:]|\.\.') -and (Test-Path -LiteralPath $path -PathType Leaf)
            if ($ok) {
                $bytes = [System.IO.File]::ReadAllBytes($path)
                $head = "HTTP/1.1 200 OK`r`nContent-Type: application/octet-stream`r`nContent-Length: $($bytes.Length)`r`nConnection: close`r`n`r`n"
                $hb = [System.Text.Encoding]::ASCII.GetBytes($head)
                $stream.Write($hb, 0, $hb.Length)
                if ($method -eq 'GET') { $stream.Write($bytes, 0, $bytes.Length) }
                Write-Host "$(Get-Date -Format HH:mm:ss) 200 $method /$name ($($bytes.Length) bytes)"
            } else {
                $hb = [System.Text.Encoding]::ASCII.GetBytes("HTTP/1.1 404 Not Found`r`nContent-Length: 0`r`nConnection: close`r`n`r`n")
                $stream.Write($hb, 0, $hb.Length)
                Write-Host "$(Get-Date -Format HH:mm:ss) 404 $line"
            }
            $stream.Flush()
        } catch {
            Write-Host "request failed: $($_.Exception.Message)"
        } finally {
            $client.Close()
        }
    }
} finally {
    $listener.Stop()
}
