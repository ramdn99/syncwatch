# Cross-network test checklist

Two PCs on **different networks** (for example home broadband and a phone hotspot). This covers
what the in-process tests, `net-lab.ps1` and the two-window labs cannot: real NAT, real
latency, and a relay used for real. Plan about 45 minutes. Keep the relay-only part short.

## Before you start

**A signaling server both PCs can reach.** The app accepts `ws://` only for localhost; any
other address needs `wss://` with a valid certificate. Signaling is still local only (your
decision: no deployment yet), so this test waits until one of these exists:
- the deployed signaling server (Milestone 9 or later, when you decide); or
- a temporary TLS tunnel to the local server (`cargo run -p syncwatch-signaling` on one PC),
  from a tunnel service you choose. The signaling server never sees passwords, credentials,
  chat or media, but the tunnel provider sees the signaling traffic, including each side's IP
  addresses and room codes. Close the tunnel right after the test.

**The app on both PCs** (no installer before Milestone 9). On the dev PC:

```
pnpm --dir apps/desktop tauri build --no-bundle
```

Then copy into one folder, and copy that folder to the second PC:
- `target\release\syncwatch.exe`;
- `target\release\libmpv-2.dll`;
- `target\release\fonts`;
- `target\release\vulkan`;
- an empty file `portable.flag`, to keep everything in that folder.

A plain `cargo build --release` does not work for this: that exe loads its UI from the dev
server. The steps above were checked on 2026-11-18 with `scripts/dev/portable-check.ps1
-Release`, which starts the exe with the dev server not running.

**Media:**
- the same local video file on both PCs (any file; the identity check compares them);
- a small, clearly licensed open torrent, for example Blender's "Sintel" (CC BY 3.0), which
  was used for the public check in ADR 0011.

**On each PC:**
1. Set up the relay in Settings, Connection (API key or ICE servers tab).
2. Point the app at the signaling server and start it, in PowerShell 7 inside the app folder:

```
$env:SYNCWATCH_SIGNALING_URL = 'wss://<signaling-host>/ws'
.\syncwatch.exe
```

**Metered quota.** Note the usage shown in the Metered dashboard before you start. Write down
only the number, never keys, credentials or the app name. Only control messages, chat and
added subtitle or audio files pass through the relay; the video never does. A relayed session
without file transfers therefore uses very little.

## Tests

Record: pass or fail, the time it took, the link type and latency shown in the members list,
and for any failure **Copy diagnostics** from both PCs (it removes credentials, IP addresses
and paths).

| # | Test | Expected |
|---|---|---|
| 1 | **Join, no password.** A creates a room; B joins with the code. Compare the four words by voice. | B is in the room within about 5 s. The words match. The members list shows "Direct · NN ms" or "Relayed · NN ms". |
| 2 | **Join with a password.** Wrong password once, then the right one. | A clear wrong-password message, then joined. No words are shown. |
| 3 | **Local file sync.** Both choose the same file ("Choose my file" on B). The host plays, pauses, seeks, sets speed 1.25×; then B does the same with the permission. | Both follow every command. After a seek both resume together (ready gate). There is no visible drift after 5 minutes. |
| 4 | **Tracks.** The host switches audio and subtitles, sets a subtitle delay, and adds a small external subtitle file. | B follows immediately and gets the file. "Tracks in sync" on both. |
| 5 | **Chat.** Messages both ways, including one with a link. Click a time chip. | Messages appear on both with time chips. The link shows a copy button and is not clickable. The chip seeks the room. |
| 6 | **Torrent room.** The host changes the source to the Sintel magnet; B confirms (name and size shown). Play, then seek far ahead. | Both download and play. The ready gate waits at the seek and shows what B waits for. At 100%: "Downloading and sharing have stopped. No further traffic." |
| 7 | **Interruption.** Turn off Wi-Fi on B for about 10 s, then on. | "Connection interrupted. Reconnecting..." appears only during the outage and clears by itself. B keeps its seat and catches up. |
| 8 | **Relay only (short).** On B: Settings, Network, **Always use relay**. Leave and rejoin. Repeat tests 3 and 5 for 3–5 minutes. Then turn it off again. | The members list shows "Relayed". Sync and chat work as before. Stop after 5 minutes. |
| 9 | **Host leaves.** A closes the window (or ends the process). | B becomes host after the grace period (succession) or sees a clear end message. |

## Afterwards

- Note the Metered usage again; report the difference.
- Close the tunnel or stop the signaling server, and close both apps.
- Add the results, with dates and network types (no IP addresses), to this file or to the
  Milestone 9 test report.
