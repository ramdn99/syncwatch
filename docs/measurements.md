# Size and resource measurements (Milestone 9)

Measured on 2026-11-20 on the development PC (Windows 11, 8 logical cores), with no mouse or
keyboard input.

## Package

| File | Size |
|---|---|
| `SyncWatch-0.1.0-x64.msi` | 262.1 MB (274,829,312 bytes) |
| of which the WebView2 offline installer | 205 MB (used only when Windows lacks WebView2) |
| `SyncWatch-Portable-0.1.0-x64.zip` (not attached to releases) | 57.8 MB |

The source is the dry run of `scripts/release.ps1` (`dist\release\0.1.0`), whose output was
checked by `verify-package.ps1`.

## Idle CPU and memory

Command: `scripts/dev/idle-measure.ps1`.
- Two SyncWatch windows on one PC (host a and member b, direct link, no relay).
- Optimized build (Cargo profile `measure`: release optimizations plus the dev automation).
- Each window is measured together with its WebView2 processes (UI and bridge; 9 processes
  per window).
- CPU is given as a percentage of **one** logical core.
- "Private" is private bytes summed over the window's processes. The working set counts
  shared pages once per process, so it overstates the total.

| Situation | Window | CPU (one core) | Private memory | Working set |
|---|---|---|---|---|
| Room open, nothing loaded (84 s, b minimized) | host a | 2.1% | 331 MB | 791 MB |
| | member b | 2.6% | 335 MB | 795 MB |
| Torrent finished at 100%, the finished file **playing** in the room (114 s) | host a | 18.6% | 728 MB | 1,197 MB |
| | member b | 12.1% | 738 MB | 1,209 MB |

Notes:
- **What the torrent row shows.** The video plays: the lab file is small (320x180, 5 fps,
  H.264, two AAC tracks), so decoding alone does not explain 12-18% of a core. The rest is
  probably rendering at the display rate, the UI updating the playing position, and the
  room heartbeats. Splitting that up (a local file playing in a room, without a torrent) is
  an **open item**. Both windows reported no failed check in that run. The
  after-100% traffic checks (no peer, tracker or UDP socket) are proven by the regular torrent
  lab runs (`docs/checklists/torrent-stop.md`).
- **The UI ran in development mode.** These runs load the UI from the Vite dev server (React
  development mode), so the shipped build is expected to use somewhat less.
- **Two windows share one PC.** Both instances ran on the same machine and competed for it.

To repeat:

```
pwsh -NoProfile -File scripts/dev/idle-measure.ps1 -Part room -Seconds 120
pwsh -NoProfile -File scripts/dev/idle-measure.ps1 -Part torrent -Seconds 120
```
