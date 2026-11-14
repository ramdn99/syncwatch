# ADR 0012: Ready means "has the data I need now"; the buffering popup is a real stall

Status: accepted (fix after the Milestone 7 manual test). Date: 2026-11-12.

## Problem (manual test, torrent room)

1. **False gate.** The seek bar showed the current part downloaded for everyone, yet the room
   showed "Waiting for X (downloading)" or "(buffering)" and did not play.
2. **False popup.** While the video played normally for the members, a "Buffering" popup still
   showed on their screens.

## Root causes

### Symptom 1: the gate held a member whose current part was on disk

The suspects from the bug report, checked one by one:

- **(g) The cause.** The gate's "buffering" was mpv's raw `paused-for-cache` flag, reported to
  the host at once, so every brief cache toggle while playing paused the whole room. The host
  then:
  1. paused everyone and opened the gate;
  2. released it, so each member seeked to the paused position and resumed;
  3. the resume itself toggled the cache flag, and the cycle began again ("Waiting for X
     (buffering)", over and over).

  A pure reproduction with only the old API (`crates/sync-core/tests/torrent_gate_repro.rs`:
  five 0.3 s toggles while playing from data on disk) FAILS on the old code (104 gate events,
  10 buffering reports, the room paused) and passes after the fix.
- **(c) A stale ready.** A member could be marked ready for a new epoch while its seek was still
  running. Before, the raw buffering flag hid this.
- **(d) The window followed the member's own player, not the room.** The engine learned about a
  room seek only from the player's seek command, and only once the player had opened the stream.
  A member that was loading, joining late or waiting at the gate therefore:
  - kept its playhead at byte 0, which is where its "playable" check was made;
  - fetched from the start of the file instead of from the room's position.
- **(a), (b): "downloading" in the gate.** With "Wait for everyone" on, a member that was not
  yet "playable" (judged at byte 0, see (d)) reported `Downloading` and held the gate with the
  reason "downloading". That is the overall torrent state, not coverage at the room position.
- **(f) Partly.** The time-to-byte map needs the duration. A member without a player had none,
  so the proportional mapping gave byte 0.
- **(e) Not the cause.** "Playable" needs the head, the tail and the pre-buffer, never the whole
  file.

### Symptom 2: the popup during normal playback

- **(k) The overlay read the wrong state.** The torrent overlay ("Buffering N%") was bound to the
  engine's `plan.buffering`. The engine computed it at the policy playhead, which is the loopback
  server's read position. That is mpv's demuxer read-ahead, and it sits at the download
  frontier for the whole time a stream downloads. So "the 4 MiB after the read position are
  incomplete" was true nearly all the time, even with minutes buffered ahead of the playback
  position.
- **(g) The pill.** A second "Buffering..." pill was bound to mpv's `paused-for-cache` flag
  directly, and flickered with it.
- **(h)/(j)** The overlay cleared only when the engine's frontier window filled.
- **(i)** It was this window's own state (not another member's), but the wrong state.

## Decision

- **Readiness at the room position** (`crates/torrent/src/ready.rs`, pure). A participant is
  ready when all of these are on its disk:
  - the window from 1 s before the room position to 5 s after it (at least 2 MiB, at most
    24 MiB);
  - (superseded by the revision below: no head and tail; the container ranges the player
    read instead).

  Overall progress never counts.

  Hysteresis: a participant becomes ready on the full window, and stops being ready only when
  even a 1.5 s keep window is missing.

  The engine reports `TorrentStatus::window` (`ready`, `at_ms`, `missing`, `permille`). The
  time-to-byte map is VBR-aware: it learns from seek samples, otherwise it falls back to the
  duration. Without any mapping the readiness is unknown, never a guess at byte 0.
- **The gate uses only this** (sync-core `Local::needs_data`):
  - a torrent member needs data when its window is missing;
  - any player needs data after a real stall longer than the popup's show delay.

  `MemberStatus` carries `wait: Option<BufferWait>` (position, missing bytes, window progress).
  `NotReadyReason::Buffering(Option<BufferWait>)` gives the host "Waiting for X: buffering at
  12:30 (needs ~3 MB more)" and the members list "buffering at 12:30, 60% of the window". The
  gate timeout and "Resume without X" are unchanged; an excused member does not pause the room
  again until it was ready once.
- **The room's part first.** Sync-core emits `Effect::TorrentTarget` for each of these:
  - at the start of a torrent (before the player exists);
  - on every new room epoch (seek, pause, resume): a re-target;
  - on every heartbeat (a refresh).

  The engine extrapolates the position while the room plays. `PiecePolicy` gets a `Room` cursor
  at the first missing byte of the room window. It runs first and alone, plus the head and tail
  while the file cannot be opened yet. After that, the existing order continues: lead, then
  tail, then fill. "Playable" is judged at the room position.

  The host's heartbeat carries its media duration, so a member without a player can map the
  room position to bytes.
- **Buffering popup** (`crates/sync-core/src/stall.rs`, pure) for THIS window's player only.
  - **Input:** the player paused for cache while it should play (latched through the pause the
    room makes for it), or the window at the room position is missing while the room plays or
    waits at the gate.
  - **Delays:** it shows after 700 ms and hides 400 ms after the condition ends.
  - **Guard:** it never shows while the position advances by playback (a seek jump does not
    count).
  - **Transitions:** these close it at once, without the hide delay, unless the condition still
    holds: seek done, resume, media change, reconnect, torrent complete (the handoff to the
    finished file), stream start.
  - **Other members:** another member's buffering is the small banner "Waiting for X to catch
    up", never the popup.
  - **Outside a room:** the raw mpv pill stays only outside rooms.
  - **Banner delay:** the gate banner shows after 600 ms, so the short gate pass of every seek
    does not flash.
- **mpv cache options** (`crates/player/src/mpv.rs`):

  | Option | Value | Why |
  |---|---|---|
  | `cache-pause` | `yes` | A real stall pauses cleanly instead of stuttering. |
  | `cache-pause-wait` | `2` | After a stall, resume only with 2 s buffered, so data arriving at about the playback rate does not toggle pause on and off. |
  | `cache-pause-initial` | `no` | No pause before the first frame. |
  | `demuxer-readahead-secs` | `20` | Local files read 20 s ahead. Streams use the cache, which reads ahead as far as the download allows. |

- **Protocol version 2** (`MemberStatus::wait`, `MemberInfo::wait`, `Heartbeat::duration_ms`,
  `NotReadyReason::Buffering(..)`).

## Evidence

- **Pure reproduction** (`torrent_gate_repro.rs`, only the old API): with five 0.3 s cache
  toggles while playing from data on disk, the old code FAILED. The host opened the gate 104
  times, the member reported buffering 10 times, and the room paused. The new code passes.
- **Old build, two windows** (`torrent-lab.ps1 -Scenario torrentgate -SameIpDefault`):
  - (a) with both windows partial (34% and 25%) and playing from data on disk, the host's window
    showed the buffering popup in 76 of 76 samples while the video advanced;
  - the gate did not hold in that 20 s window; its loop is reproduced by the pure test above.
- **New build, same lab**, every check passing on both windows:
  - **(a)** Both windows partial (62% / 59%), 77 samples each: popup 0, gate 0, position
    advancing.
  - **(b)** Seek to 50:00, missing for the member. The host's gate read "buffering at 3000 s, 80%
    of the window, needs 2.1 MB". The member's popup showed during the stall. Its room window
    arrived in 6.1 s, as a new range at 58.2–60.2 MiB (not continuing from the start). After the
    room resumed in sync: popup 0 on both windows.
  - **(c)** Seek back to 1:00: popup 0. The gate was active for one 250 ms sample ("loading"),
    under the banner's 600 ms delay.
  - **(d)** The member rejoined late while the room was paused at 40:00. Its first ranges were
    the head and tail. The room window (46.5 MiB) was there after 5.3 s, at 5% overall.
  - **(e)** After 100% and the handoff to the finished file: popup 0 over 10 s on both windows.
- **Engine test, throttled seeder** (`a_member_without_a_player_fetches_the_room_position_first`):
  - the room was paused at 80% of a 64 MiB file, and the member had no player;
  - it was ready there after 3.2 s with 8.2 MiB downloaded: the head, 51.0–53.2 MiB and the
    tail.

## Revision (2026-11-14): only the playing part first; container bytes on demand

### Problem (manual test)

The host played, but the member stayed at "buffering" and never started. Its log shows the
torrent added at 09:45:49 and then no "file loaded": its player never opened the stream.

### Root cause

The suspects from the bug report, checked one by one:

- **(a)/(c) The cause.** Opening the stream needed all of these on disk, and the player was
  opened only after that ("playable"):
  - the file head (2 MiB);
  - the file tail (8 MiB);
  - a 4 MiB pre-buffer at the room position.

  Only the 2 MiB room window was fetched, and the room position moved with playback. Nothing
  fetched the rest of the pre-buffer: the lead cursor sat at byte 0 because the player had never
  read anything. So the member downloaded the file front-to-back from 0 and opened only when
  that download reached the room position.
  - **Engine test:** `a_member_joining_a_playing_room_opens_and_keeps_up` (room playing from
    5:00, throttled seeder) FAILED on the old code. The stream became playable only after
    21.2 s, with 0–36 MiB and 56–64 MiB on disk. After the fix: 0.1 s.
  - **Two windows:** `torrent-lab.ps1 -Scenario torrentplay -SameIpDefault` on the old build.
    The member rejoined while the room played at 40:00 and never played within 90 s. Its ranges
    were 0.0–52.0 MiB (front to back) and 65.7–73.8 MiB (the tail).
  - **Why the first lab missed it:** in that lab the room was paused at 0:00 when the member
    started, where byte 0 and the room position coincide.
- **(d)** Not the cause: the duration reached the member through the heartbeat.
- **(b)** Not a starvation of player requests: the loopback server's own librqbit stream serves
  them.
- **(e), (f), (g)** Not observed.

### Decision

- **Order** (`PiecePolicy`, cursors in priority order):
  1. `Room`: the first missing byte of the room window.
  2. `Demand`: the byte the player is blocked on outside the window (the loopback server now
     reports its open responses). Kept only while that piece is missing.
  3. `Lead`: the player's read-ahead, from the playhead, or from the room position before the
     player read anything.
  4. `Fill`: the rest of the file from the playhead, wrapping around, after the read-ahead has
     been full and calm.

  While `Room` or `Demand` is needed, nothing else runs. Each room seek, resume, pause or
  media change moves the window at once; the old window drops to normal priority. There is no
  head or tail cursor any more.
- **Open at once, at the room position.** The stream is offered as soon as the torrent is added.
  Sync-core loads it with `start=<room position>` (`loadfile <url> replace -1 start=…`). mpv
  reads its header and index on demand, then goes straight to that position. The member stays
  "downloading" (never blocking the room) until the player has opened the file, then it is
  "ready".
- **Readiness** = the room window plus the container ranges this file's player actually read.
  The engine records them: reads made while opening, and short reads away from the room's part
  afterwards. They are logged ("player opened the stream; ranges it read to open"). Before the
  player opened the file, the start-up range `[0, startup_bytes)` (default 1 MiB, configurable)
  stands in for them. There is no head or tail requirement.
- **Torrents are added paused** and unpaused once our streams are open. Otherwise librqbit's own
  order (first piece, last piece, then sequential) would start before the room window.

### Found in the two-window runs and fixed

- **A member whose player has just opened never pauses the room.** It opens within
  0.3–0.6 s, often before its window arrives. Being "ready" but still buffering, it paused
  everyone for about 0.5 s. The host now excuses such a member until it has been ready once,
  like any late joiner.
- **Container ranges come from a log of every request.** The engine ticks every 200 ms, and the
  server's read report keeps only the latest value. mpv's open-time reads take milliseconds each
  (header, then index, then the position), so they were missed. The loopback server now keeps
  a bounded span log.
- **The VBR sample of a room seek comes from where playback resumed.** With `start=` opening,
  mpv reads the MKV Cues lazily at the first seek. That index read at the end of the file was
  taken as the time-to-byte sample, so 50:00 mapped to the file's end, which was already on
  disk. Readiness said "ready", and the gate showed only "loading". The sample now comes from
  the first request after the seek that reads at least 1 MiB.
- **The host offers its metadata and port only after its torrent is unpaused.** A member
  connecting to a torrent that is still paused is refused, and librqbit backs off before trying
  that peer again.

### Lab evidence (final code, one lab at a time)

- **`torrentplay`, old build:** the member that rejoined at 40:00 never played in 90 s, and
  downloaded front to back (0.0–52.0 MiB plus 65.7–73.8 MiB).
- **`torrentplay`, new build:** all checks (a) to (e) passed on both windows.
  - **(a)** The member's player opened 0.36–0.58 s after it accepted; one minute in sync (worst
    196–294 ms); no popup, no gate.
  - **(b)** At 55:00 the new data arrived as a separate range at 64.0 MiB (the new position).
  - **(c)** The late joiner played 0.24–0.34 s after accepting.
  - **(d)** The seek back to 40:10 played again within 0.33–0.5 s.
  - **(e)** No popup after 100%.
- **`torrentgate`:** all checks (a) to (e) passed. In (b) the gate read "buffering at 3000 s,
  10% → 30% of the window, needs 2.1 MB".
- **The M7 regression** (`torrent-lab.ps1`): 29/29.

### Open issue

When the host and the member start the same torrent at the same moment (the member accepted
before the host had the file list), the member got nothing after its first 238 KB in 2 of 5
`torrentgate` runs. It found a peer again only on librqbit's peer retry, 15–35 s later (log:
"no peers: retrying peer discovery"). During that time the popup showed, correctly.

The member's link to the host's engine on the same PC apparently drops, and qBittorrent
refuses it as a second connection from 127.0.0.1. The exact cause is not pinned down. It did
not happen in `torrentplay` (the reported bug's flow: the host plays first) in 4 of 4 runs.

### Container ranges, measured with mpv

`cargo run -p syncwatch-player --example range_probe -- <file> <start_s> <seek_s>`: a
throttled (4 MiB/s) local range server and real libmpv with the app's cache options. Each file
is opened with `start=` at 5:00 (`test.mkv` at 15:00) and then sought.

| File | Read to open at the position (besides the playback data there) | Seek |
|---|---|---|
| MKV, Cues at the end (`cues-end.mkv`, 420 MB, 10 min) | 192 KiB at the start, the last 11 KB (Cues) | 1.5 MiB at the target only |
| MKV, Cues at the start (`cues-front.mkv`, `-reserve_index_space`) | 128 KiB at 0 and 128 KiB at 301 KB (the index) | 1.5 MiB only |
| MP4, moov at the end (`moov-end.mp4`) | 192 + 128 KiB at the start, the 433 KB moov at the end | 1.5 MiB only |
| MP4, moov at the start (`+faststart`) | 512 KiB at the start (the moov is inside) | 1.5 MiB only |
| `D:\media\test.mkv` (1.29 GB, 28 min, mkvmerge) | 128 + 192 KiB at the start, 420 KB at the end | 1.5 MiB only |

Every file opened and seeked with on-demand ranges only. None needed a larger range. The
largest start-up read was 512 KiB, so the default start-up range is 1 MiB.

### Limits

- **librqbit's stream look-ahead.** librqbit queues the 32 MiB after every open stream. When
  the player reads its header, the pieces right after it are queued too: about 1–1.5 MiB arrive
  before the read ends (measured in the engine tests).
- **librqbit's own order.** When no open stream has a piece a peer could serve, librqbit takes
  the first piece, then the last piece, then sequential pieces. This never comes before our
  window: the priority pieces are always tried first. It only uses spare capacity.
- **Large moov atoms.** The moov of a very long MP4 can be several MB. It is fetched on demand
  when mpv asks for it; only the readiness estimate of an unopened member assumes 1 MiB.

## Consequences and limits

- A real stall now pauses the room after about 0.7–1.0 s instead of at once (the show delay).
- The window size is in config (`EngineConfig::ready`), not in the Settings screen.
- The readiness of a member that has not opened the file depends on the host's duration (the
  heartbeat). Until it arrives (at most 1 s), its readiness is unknown, and the gate treats it
  as before (loading).
