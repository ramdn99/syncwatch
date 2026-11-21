# ADR 0007: Real-player sync, shared tracks, external files, chat timestamps, host succession

Status: accepted (Milestone 6). Date: 2026-11-06.

## Context

Milestones 2 and 5 built the deterministic sync-core machine and the room session, but the app
still drove mpv directly: in a room nothing went through the room timeline. Spec sections 6, 7
and 9 and Milestone 6 require:
- synchronized play, pause, seek, speed and media changes, with the requester going through the host;
- a ready gate with a UI;
- media identity and the mismatch flow;
- chat timestamps tied to playback;
- host-only shared tracks and external subtitle/audio files sent by the host;
- late joiners;
- the popup-restores-minimized-window regression;
- host succession over the network.

## Decisions

### The real player behind sync-core (`apps/desktop/src-tauri/src/roommedia.rs`)
- **Observations go to sync-core:**
  - `FileLoaded` (tracks and duration);
  - `Tracks`;
  - `SeekDone` (mpv `playback-restart`, a new player event);
  - `LoadFailed`;
  - a `Position` poll every 100 ms: time-pos, paused, paused-for-cache.
- **Commands go to mpv:** load, unload, pause, seek, speed, track selection, delays, add external.
- **In a room every control is a request:** play, pause, seek, speed, track and delay changes,
  and shortcuts. The host validates it and broadcasts one command with an epoch, and every
  window, the requester included, applies it. A member's own machine already refuses
  requests it has no permission for; the host refuses forged ones (sync-core tests).
- Player state never generates requests (`player_observations_never_generate_requests`), so a
  remote command cannot echo back.
- Scheduled starts, heartbeats, clock sync and drift correction are the sync-core logic of
  ADR 0003, now with real observations:
  - under 0.15 s is ignored;
  - up to 2 s is corrected with a speed nudge between 0.97 and 1.03;
  - beyond that, a hard seek.

### Chat timestamps
- **Root cause of "every message shows 00:00":** playback never went through the room, so the
  host's room timeline stayed at its initial paused position 0. Every message was stamped 0 once
  a source existed.
- **Now:** the host stamps each message when it receives it, using the room timeline
  extrapolated with the room's paused state and speed (`position_at(now)`). The host's own mpv
  follows that same timeline within the drift bounds.
- A member's claimed position (its real player's extrapolated position) is used when it is
  within 5 s of the host's value. While paused, the paused position is used. Without media there
  is no timestamp at all.
- Every message carries its media epoch. After a media change, older messages keep their chips,
  greyed out and not seekable.
- **Chips:** `mm:ss`, or `h:mm:ss` when the video is an hour or longer; "no timestamp" otherwise,
  never a placeholder `00:00`. Clicking a chip requests a seek to the stamp minus a 2 s lead-in
  (needs the host role or can_seek).
- Also: seek-bar markers, jump to latest with an unread count, "Near now" (plus or minus 2 min),
  the spoiler guard ("Hidden until 15:00"), the "Now playing" divider and the fullscreen overlay.

### Shared tracks
- **Host only.** The host picks a local track, and the app turns it into a `TrackRef`: ff-index
  first, then kind, language, title and codec, or an external id. sync-core resolves it on every
  member and applies it immediately, without a seek. Members' lists are read-only. Late joiners
  and reconnecting members get the current state.
- **External tracks:**
  - mpv's per-track `external-filename` maps each one back to its room `external_id`;
  - externals are added with mpv's `auto` flag, never `select`, so adding a file never overrides
    the host's selection (the first run showed mpv auto-selecting).

### External files
- **Host side:**
  - extension allowlists;
  - ceilings of 1 GB for audio and 20 MB for subtitles, with a "No size limit" option;
  - text subtitles are converted to UTF-8 (BOM, valid UTF-8, else chardetng 1.0.0 with
    encoding_rs 0.8.42);
  - the file is copied into the per-room cache and hashed with blake3.
- **The per-room cache:**
  - its location is `<cache>\rooms\<n>`;
  - it is deleted when the room ends;
  - leftovers are deleted at startup (after a crash);
  - every pending upload and download goes with the room (the ADR 0006 reset hook).
- **Transport:**
  - chunks go on a separate reliable data channel (`Lane::Bulk`, negotiated id 2), so they never
    queue in front of control and chat on the same SCTP stream;
  - they are paced only by the bridge's bulk credit (`can_send_bulk`), with no rate limiter;
  - resume after a disconnect requests the received offset;
  - identical files already in the room cache are not transferred again;
  - subtitles go first (ADR 0003).
- **Relayed members:** if a file larger than 25 MB would go to members whose path is relayed,
  the host chooses "Send to everyone", "Skip relayed members" or "Cancel". A skipped member gets
  `FileWithheld` instead of data (M7 Part B): it stops waiting, never asks again, drops any
  partial data, its track falls back, and the file shows "The host did not send this file to you".
- Members see each file's name, size and progress and can cancel it. The host then sees
  "<member> declined <file>".

### Ready gate, media identity, late joiners
- **The gate banner** lists who is being waited for and why, and the host can resume without
  them. A member missing the file never blocks the others unless the host chose "Wait for
  everyone" (sync-core tests and the `late` scenario).
- **Choose-my-file** shows once per media epoch. A mismatching file shows the warning bar with
  "Watch anyway" (`ProceedWithMismatch`).
- **A late joiner** gets the snapshot, starts paused ("Joining... catching up"), loads and
  verifies its file, applies the track selections when the file has loaded, then seeks to the
  extrapolated position.

### Popup restores a minimized window
Not reproducible. The video surface never shows the video window while the main window is
minimized (`decide()`, the M1 Win32 test `popup_does_not_restore_minimized_main_window`). The new
`popup` scenario minimizes a member through the window API and has it load and play a file;
the window stays minimized with the video window hidden, and the video shows again after a
restore.

### Host succession after a host crash
- **Detection.** Signaling reports `HostGone` when the host's socket closes. A member acts only if
  its own link to the host is also dead. A host that only lost signaling but still has a healthy
  data channel is never replaced (tested).
- **Election.** Deterministic rank: the connected non-host members sorted by seat number. The
  member of rank r sends `ClaimHost` after (r + 1) x 8 s. The signaling server accepts the first
  claim while the room has no host, refuses the rest, and gives the new host a resume token.
  Everyone else gets `HostChanged`.
- **The designated successor is kept updated** (M7 Part B). The host sends
  `Command::Succession(SuccessionState)` to the rank-0 member (the lowest connected seat). It
  carries every kept seat (id, identity, name, custom permissions), the default permissions, the
  mute list, the ban list and the lock. It is sent again on every change and whenever the
  successor changes, for example when the old one leaves. Only the successor gets it: other
  members never see the identities and the ban list.
- **Takeover.** The successor builds a `HostHandover` from what it knows:
  - media descriptor, playback timeline (converted to its own clock), track state and manifest,
    chat settings;
  - the room rules from the succession state. The other seats are kept as reconnecting for the
    usual retention time, so each member gets its seat and custom permissions back. Mutes stay,
    a banned identity is refused, and a locked room stays locked;
  - chat ids continue above the highest it has seen;
  - its downloaded external files become the files it serves.

  **Not carried:** the chat history for later joiners (everyone keeps what they already see).
  If rank 0 crashes together with the host, rank 1 takes over without the rules (it never
  received them), as before.
- **Reconnect.** The others forget the old host's identity and clock (`ExpectNewHost`),
  reconnect and authenticate. Password rooms: the new host must prove the room password.
  Rooms without a password: new verification words. The same media counts as a reconnect, so
  the loaded file, the downloads and the choose-my-file state are kept.
- **A host that comes back after a takeover** gets `BadToken` and leaves with "The room continued
  with a new host while you were disconnected."
- **Fallback.** If no member is left, the room closes after the 30 s grace period as before.

### Dev tooling
- **Scenarios** in `two-windows.ps1`: `sync`, `chattime`, `late` (3 windows), `popup` and
  `succession` (3 windows; window a's process is killed). They run through the real UI command
  path and the real player.
- Every chat text in them carries a marker, and the script greps all logs for it and for the
  credential values.
- `make-test-media.ps1` builds:
  - a 63-minute video with 2 audio tracks and a subtitle;
  - a Windows-1252 SRT and an external audio file;
  - the mismatch copy (copied from `test.mkv`, then only the copy's title tag edited).

  The original is never written.
- Debug builds with a dev identity use a separate WebView2 user-data folder per instance.
  Otherwise all instances share one WebView2 browser process, and killing one window took the
  others' web views down (found by the succession test).

## Measurements (two or three windows on one PC, direct; debug build)

| Check | Result |
|---|---|
| Host play -> visible in the member's player | 340-408 ms (includes the scheduled-start lead time) |
| Host pause / seek / audio switch -> member | 15-117 ms / 37-106 ms / 88-91 ms (100 ms polling included) |
| External subtitle selected by the host -> member | 95-175 ms (M7 Part B; was 1.3-4.9 s, see below) |
| Host subtitle delay +500 ms and audio delay -300 ms -> member | 53 ms; the reset to 0 also applies |
| Position agreement while playing (paired samples) | worst 30-84 ms |
| Late joiner (mismatch file, proceeded) vs host | worst 56 ms; same audio, subtitle and delay; identical chat chips for the history |
| Chat chips at 0:10, 5:30, 1:02:03 (playing, 1 h+ video) | stamp minus the member's real position: +47, -55, -115 ms (5 fps test video: time-pos moves in 200 ms steps) |
| Two simultaneous senders | the same total order in both windows; ids increasing |
| Host crash -> the next member hosts, the other reconnected | about 8.2 s (8 s deliberate wait + about 160 ms); survivors keep playing, 42 ms apart |
| Log grep (debug level, all windows) | no chat content, no credential values |

## External subtitle selection latency (M7 Part B)

The member applied a just-added external subtitle 1.3-4.9 s after the host selected it. There
were two causes:
- **Control behind bulk.** The host's selection is a small reliable message, but it was queued
  in the bridge behind up to 4 MiB of external-audio chunks. Logs showed it reaching the member
  3.9 s after it was sent. Fixes:
  - the bulk window is now 512 KiB (`BULK_HIGH_WATER`);
  - the page reports fresh credit below 64 KiB (`LOW_WATER`). It must stay well below the
    window, or the credit event never fires and transfers stall (seen with 512 KiB / 1 MiB);
  - in the app driver, bulk chunks get their own queue and a biased select, so control is
    served first;
  - a step that only stored a chunk publishes the UI state at most every 100 ms.

  Direct bulk throughput rose from 13-14 to 21.8 MB/s (a 1 MiB window measured 13.1 MB/s).
  Relay through the local coturn measures 6.1-7.0 MB/s on this machine today with both the old
  and the new settings, so it is unchanged (ADR 0005 recorded 8.1 MB/s earlier).
- **The check itself.** The member's scenario waited for BOTH external files to be in its player
  before taking the subtitle timestamp, so the external audio download time was folded into the
  number. The subtitle is now measured on its own, and the delay check waits until both files are
  in.

Also, mpv's `aid`/`sid` values are now authoritative for the selected flags
(`tracks::apply_selection`), so a stale `track-list` read cannot report the previous track. It
was not the cause here, but it is the correct reading.

## Consequences
- Milestone 7 plugs the torrent engine into the same effects (`StartTorrent`, `StopTorrent`,
  `LoadTorrentStream`) and adds its state to the room reset.
- Closed in M7 Part B: the stalled download for skipped relayed members (`FileWithheld`), and
  succession losing bans, the lock, custom permissions and mutes (`SuccessionState`).

## Revision: Host controls come from the host's state (Milestone 9, 2026-11-21)

**Finding.** After collapsing and re-opening the Host controls section, three values showed
defaults, because they were kept only in the component while the room kept the real values:
- the playback preset;
- "Wait for everyone before playing";
- the subtitle and audio delays.

**Fix:**
- The host reports `HostSettings` (preset derived from the default permissions, wait for
  everyone, delays, lock) as a UI event whenever they change and after becoming host. The
  Host controls render only from it.
- "Wait for everyone" now moves with the host role (`HostHandover`, `SuccessionState`;
  protocol 4).

**Where each control lives:**

| Control | Stored in | Members and late joiners | Moves with the host role |
|---|---|---|---|
| Preset / default permissions | host `default_perms` | each member's permissions in the snapshot | handover and successor state |
| Per-member permissions | host member entries | that member's permissions | handover and successor seats |
| Wait for everyone | host gate | not needed by members | handover and successor state (protocol 4) |
| Subtitle and audio delay | track state | snapshot and `Tracks` commands | handover (tracks) |
| Lock | host `locked` | not needed by members | handover and successor state |
| Chat on, slow mode | chat config | chat state in the snapshot | handover (chat config) |
| No size limit for added files | the host window (its own uploads); default from Settings | not needed by members | stays with that window |

**Tests:**
- `crates/sync-core/tests/host_settings.rs`;
- `HostControls.test.tsx` (collapse and re-open);
- the real window (Only me, wait, 250 ms and lock survive collapse and re-open).
