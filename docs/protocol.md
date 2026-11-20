# Protocol overview

How SyncWatch instances talk to each other. The details and the reasons are in the ADRs
named in each section; the code is the reference.

## Layers

| Layer | What | Where | ADR |
|---|---|---|---|
| Joining | Copy-and-paste invite (`SWINV1-...`) and reply (`SWREP1-...`) carrying the WebRTC offer/answer, signed, compressed, checksummed; no credentials | `crates/net/src/invite.rs` | 0014 |
| (dev and tests) | WebSocket signaling server, JSON messages | `crates/protocol/src/signaling.rs`, `crates/signaling-server` | 0005 |
| Transport | WebRTC data channels in a hidden WebView2 window; binary IPC frames between it and Rust | `crates/net/src/frame.rs`, `apps/desktop/src/bridge/bridge.ts` | 0005 |
| Authentication | SPAKE2 (room password, or an empty one) plus ed25519 identities, per-session MACs | `crates/net/src/auth.rs` | 0006 |
| Room messages | postcard-encoded `Envelope { version, message }`, version 3 | `crates/protocol` | 0003 |
| Room logic | The host's pure state machine decides; members follow | `crates/sync-core`, `crates/net/src/session.rs` | 0003, 0006, 0007 |

Topology: a star. Every member has one link to the host; the host relays chat and state.

## Invite flow (release builds)

1. The host creates an invite. It holds the host's offer with all ICE candidates, the room
   id, the invite id, an expiry, the host's identity key and its DTLS fingerprint, signed
   with the host's key.
2. The member pastes it. The member checks the signature, expiry and checksum, then makes a
   reply (its answer and identity, signed).
3. The host pastes the reply. Both sides show the same four words: a hash over the room,
   the invite, both fingerprints and both identities. The host clicks **Admit**, and only then
   is the answer applied.
4. When the link opens, both sides check the DTLS fingerprint against the signed texts, then
   run the room authentication.

A link that dies for good is not restarted: the seat is kept for 5 minutes and the host
re-invites.

## Data channels

| Channel | Use |
|---|---|
| Reliable | Control and chat messages |
| Unreliable | Heartbeats and clock pings |
| Bulk | External-file chunks (subtitles and audio picked by the host), torrent metadata |

The main video is never sent: everyone plays their own copy (identified by size, duration
and a BLAKE3 sample hash) or the same torrent.

## Room messages (`crates/protocol/src/messages.rs`)

| Group | Messages |
|---|---|
| Session | `Hello`, `Welcome(Snapshot)`, `Reject` (version, full, locked, banned), `Bye` |
| Time | `TimePing`/`TimePong` (clock offset), `Heartbeat` (host position and state) |
| Playback | `Request` (play, pause, seek, speed, change media, tracks) from members; the host's decisions arrive in the snapshot and heartbeats; `Status` (member readiness: missing file, verifying, downloading, buffering) |
| Chat | `ChatSend`, `Chat`, `ChatRejected`, `Command` (delete, clear, mute, unmute) |
| Files | `FileRequest`, `FileChunk`, `FileHave`, `FileCancel`, `FileWithheld` |
| Torrents | `TorrentMetaRequest`, `TorrentMeta` (only for the room's announced infohash) |

Every frame is checked in this order:
1. size (64 KiB at most);
2. version (`PROTOCOL_VERSION` = 3; a different version is refused);
3. validation (string, list and chunk limits, `crates/protocol/src/limits.rs`).

Fuzz-style tests: `crates/protocol/tests/fuzz.rs`.

## Sync

- The host is the clock. Members estimate the offset with `TimePing`/`TimePong` and follow
  the host's position from heartbeats.
- Small drift is corrected with playback speed, large drift with a seek.
- A seek or a pause for buffering waits for members (default 15 s, Settings), with "resume
  without X".
- Numbers and tests: ADR 0007, `crates/sync-core/tests`, and `syncwatch-sim` for simulated
  latency, jitter and loss.
