# ADR 0003: Wire protocol and the sync-core state machine

- Status: Accepted (Milestone 2)
- Spec: sections 6, 8, 9, 11, 12

## Decision

### Protocol (`crates/protocol`, no I/O)
- Every frame is `Envelope { version: u16, message: Message }`, encoded with **postcard**.
  `decode()` checks the frame size (64 KiB), the version (`PROTOCOL_VERSION = 1`) and
  `Message::validate()` (limits on every string, list and chunk), in that order. New variants
  are appended at the end of the enums, so older peers fail cleanly on the version check.
- Host star: members send `Request`s, `Status`, `ChatSend`, `TimePing` and transfer requests.
  The host sends one authoritative `Command` (playback with epoch and media epoch, media change,
  tracks, permissions, members, moderation, gate, notices, kick, host transfer, room closed),
  plus `Chat`, heartbeats and `TimePong`.
- Each message has a channel and a priority. Heartbeats and clock pings use the unreliable
  channel; everything else is reliable and ordered. Priorities are control > chat >
  heartbeat > bulk.
- **No main media on the wire.** Only `FileChunk` carries opaque bytes: at most 16 KiB, and it
  always names a manifest `ExternalId`. `MessageKind::carries_bulk_bytes` is an exhaustive match,
  and `no_media_over_datachannel_enumeration` allows exactly that one kind. Media descriptors
  carry only names, sizes and hashes (`FileIdentity` = blake3 over the first and last 4 MB, plus
  size and duration). Paths are never sent.
- Magnet links are parsed authoritatively in Rust (`magnet::parse`). The shared vectors in
  `crates/protocol/tests/magnet-vectors.json` will also drive the TypeScript mirror (Milestone 3).

### Sync core (`crates/sync-core`, pure)
- `Peer::handle(Clock, Event) -> Vec<Effect>`; `Peer` is `Host` or `Member`. A host handover
  swaps the role in place and keeps the local participant.
- `RoomPlayback { position_ms, anchor_host_us, paused, speed, epoch, media_epoch }`. A state
  whose anchor is in the FUTURE is a scheduled start: every participant (the host included)
  holds it paused until the anchor, whether it arrived as a command, a heartbeat or a snapshot.
  Heartbeats only matter when they carry a newer epoch. This prevents, by construction, the old
  bug where members started 300 ms early (regression test
  `heartbeat_never_overtakes_scheduled_play`).
- Clock sync: NTP-style ping/pong on the unreliable channel, using the median offset of the
  lowest-RTT third of the last 16 samples, with a one-way heartbeat hint until the first pong.
  The lead time for scheduled starts is the highest reported RTT + 200 ms, clamped to
  300-1500 ms.
- Drift: under 150 ms ignore; 150 ms-2 s nudge the speed (0.97-1.03, proportional, with
  hysteresis down to 40 ms); over 2 s hard seek. Correction is suspended while buffering,
  seeking or holding. The player's seek latency is learned (EMA), and catch-up seeks aim ahead by
  that much; this took a late joiner's steady offset from 78 ms to about 22 ms. While paused,
  members align to within 40 ms.
- Ready gate (host): plays and seeks during playback wait for members that HOLD the media
  (ready epoch reached, not buffering). Members that are missing the file, verifying or
  downloading block only with "wait for everyone"; members who declined never block. Exits are a
  timeout (15 s default) or "resume without X". Members resumed without are excused from
  re-pausing the room until they report ready. A buffering member pauses everyone and playback
  resumes automatically.
- Tracks: the host owns `TrackState`. `TrackRef` resolves by ff-index first, then by
  kind+lang+title+codec; external refs resolve through the app-supplied `external_id`.
  Commands go to the player only when the resolved value changes. Unresolvable refs give a
  non-blocking warning. While an external file downloads, the previous track stays selected.
- External files: only manifest entries can be requested or received. Chunks must arrive in
  order and within the manifest size, and the file must match the manifest blake3. Subtitles
  preempt audio (the audio download is suspended with its hash state and resumed from its
  offset). Members can decline. Identical cached files are skipped (`FileHave`). There is no
  rate limiter: `OutboundQueue` simply serves control first.
- Chat: plain text. Control and bidi characters are stripped, the limit is 500 graphemes,
  5 messages per 5 s per member, with an optional slow mode, and there are mute, delete, clear,
  "Chat enabled" and "Hide chat from members". The host stamps the media time and honors a
  member's claim only within 5 s of the room position. History is in memory only (max 2000,
  snapshots carry 200) and moves with the host role.
- Media change: everyone stops the old source (`Player Stop`, `StopTorrent`), resets playback
  and tracks, and shows the "Choose my file" panel once or the magnet confirmation. Concurrent
  requests: the later one wins, and the earlier requester gets `MediaChangeSuperseded`.
  "Choose my file" sends no request at all, only status reports, so it needs no permission.

### Simulator
`sync-core::sim` (feature `sim`, on by default) provides skewed, drifting clocks; FakeNet with
latency, jitter, and loss/reordering on the unreliable channel; a SimPlayer; and the World
harness. Every simulated message goes through `encode`/`decode`. `syncwatch-sim` is the CLI
scenario.

## Findings while building it (kept as tests)
- Timer sentinels of `i64::MIN` overflowed in `now - last`. In release builds that silently
  disabled heartbeats and position reports. Replaced with `Option`; the tests run in debug
  (overflow checks on).
- A heartbeat may overtake the Welcome (different channels): ignored quietly, not a violation.
- A ready-gate timeout used to re-pause immediately when the still-loading member reported
  buffering. Such members are now "excused".
- `Disconnect` must flush queued messages first (Kicked/Reject must arrive).

## Open items
- Authentication (SPAKE2, verification words) and seat retention: Milestone 5.
- Host succession over the network: last item of Milestone 6 (the in-process handover exists now).
- Warning before sending large externals to relayed peers: needs the connection type
  (Milestone 4/5) and is wired in Milestone 6.
