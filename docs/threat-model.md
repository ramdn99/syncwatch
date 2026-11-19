# SyncWatch threat model

Status: complete for Milestone 8 (spec section 11). Milestone 9 adds the installer and the
updater, and a security review of this document.

## What we protect, and from whom

**Assets:**
- the user's IP address;
- the room password;
- chat content;
- the Metered credentials (API key, app name, ICE credentials);
- the user's files and paths;
- the integrity of what each player plays and shows.

**Not an asset:** the main media file. It is never sent to anyone; each person plays their own
copy.

**Parties:**
- a malicious member (inside the room, authenticated);
- a malicious host;
- a compromised or hostile signaling server;
- an outsider who knows or guesses a room code;
- the TURN relay operator;
- other programs on the same computer;
- a BitTorrent swarm (see Torrents).

**Out of scope:** malware already running as the same Windows user, and a compromised build or
installer (Milestone 9: signed release builds from `scripts/build-release.ps1` only).

## Transport and authentication (ADRs 0005, 0006)

| Threat | What we do | Residual risk |
|---|---|---|
| Eavesdropping on room traffic | All room traffic runs over WebRTC data channels (DTLS), host-star: members talk only to the host. TURN relays see only encrypted packets. | The host decrypts everything by design (see Malicious host). |
| Brute force on a room password | SPAKE2 (the password never leaves the device, and one guess per attempt): backoff up to 8 s between failures, a 10-minute ban of that identity after 6 failures, and a 60 s pause of all password checks after 20 failures in a minute. The proof covers the room code and both DTLS fingerprints. | A weak password can still be guessed slowly. The UI asks for a password or the four words. |
| Man in the middle through a hostile signaling server (dev and tests only) | Password rooms: the SPAKE2 proof is bound to both DTLS fingerprints, so a relaying attacker fails the proof. Rooms without a password: both sides show four verification words derived from the fingerprints; the host sees each member's words in the members list. | In a room without a password, an attacker who controls signaling can sit in the middle if nobody compares the words. The user guide says to compare them. |
| A compromised signaling server (dev and tests only) | It only forwards SDP and ICE candidates for one room code. It never sees passwords, credentials, chat or media. Limits: 32 connections per IP, 10,000 rooms, a maximum frame size, valid room codes only. | It sees the IP addresses and ICE candidates of everyone who connects, and which room codes meet. It can deny service. |
| Oversized or malformed messages | Every frame is size-checked before decoding: 64 KiB per protocol frame, 70 KiB per bridge IPC frame, a 1 MiB batch, and 4 KiB per auth frame. Postcard decoding with a version check (protocol v3); per-field limits (names 128 B, chat 4 KiB / 500 graphemes, titles 256 B...); at most 12 members and 32 manifest entries. Anything malformed is dropped and logged as a protocol violation. | A decoder bug in postcard or in our types stays a residual risk (fuzzing: Milestone 9 review item). |
| A stalled or hung link used to freeze the room | The watchdog shows the interruption banner after 4 s of silence and restarts the peer after 10 s; a hung bridge page is reloaded after 5 s. The ready gate times out (default 15 s) and the host can resume without anyone. | A member can delay the room by up to the gate timeout. |

## Manual invites (ADR 0014)

Release builds have no signaling server. The two signaling rows above apply only to the dev and
test signaling server.

| Threat | What we do | Residual risk |
|---|---|---|
| An invite posted in a group chat is answered by someone else | The host must click **Admit** for every reply, after comparing four words that bind the room, the invite, both DTLS certificates and both identities. An impostor's reply produces different words. Then come the optional SPAKE2 password, signed identities, and Kick and Ban. | A host who admits without comparing the words. The user guide says to compare them by voice. |
| An unadmitted or refused reply getting room data | The host applies the member's answer only after Admit. Before that, no connection exists at all. Test: `an_unadmitted_reply_never_gets_room_data`. | None known. |
| Reused or expired invites | Each invite has a random 128-bit id, works once (a second reply is refused as "already used") and expires (default 10 minutes, 1 minute to 24 hours). Expired invites close their pending connection. Tests: `reused_expired_tampered_and_unknown_replies_are_refused` and codec tests. | An invite stays usable by whoever gets it first until it is used or expires; Admit still applies. |
| Tampered invites or replies (changed SDP, candidates, room, identity) | Each text is signed by the sender's identity over a domain-separated hash. The checksum catches paste damage. When the connection opens, each side checks that the other's certificate is the one the invite or reply named, and the words change if anything was swapped. Tests: codec tamper tests and `a_connection_that_does_not_match_the_reply_is_closed`. | Someone who controls the chat channel can replace BOTH texts with their own (a man in the middle). The words then differ on the two screens, so comparing them by voice detects it. |
| Malformed or hostile pasted text (huge, decompression bombs, odd characters) | Every check runs before use: a 16 KiB text cap, a 32 KiB decompression limit, format and version, a structure check (SDP size, a DTLS fingerprint, at most 40 candidates of at most 512 bytes, no control characters, names of at most 40 characters), the signature, and expiry. Test: 3,000 mutated inputs without a panic. | A parser bug in postcard, miniz_oxide or base64. |
| Credentials in invites | Invites and replies carry only SDP (with the per-session ICE ufrag and password) and candidates, never the Metered API key or TURN usernames or credentials. Test: `invites_and_replies_carry_no_relay_credentials`. | None known. |
| IP addresses in invites | Invites and replies contain the sender's ICE candidates: local and public addresses, or only the relay's with **Always use relay** (which then also hides the host's address from the people it invites). The user guide says to send invites only to the intended person. | Whoever reads an invite or reply learns those addresses. |
| Resource abuse by a flood of invites | At most 12 open invites at a time; each pending invite holds one peer connection until it is used, refused, removed or expires. | None known. |
| A dropped member rejoining | Coming back needs a re-invite from the host, bound to that member's identity (anyone else's reply is refused as "for someone else"). The seat is kept for 5 minutes (configurable). | None known. |

## Malicious member

| Threat | What we do | Residual risk |
|---|---|---|
| Forged track or moderation commands | Track commands (audio, subtitles, delays, externals) and chat moderation are accepted only from the host identity. Anything else is rejected and logged as a protocol violation (named tests in `crates/sync-core/tests`). | None known. |
| Playback commands without permission | Members only send requests. The host checks the live permissions (play/pause, seek, change media, change speed) and broadcasts one authoritative command with an epoch; stale epochs are dropped. | A member with a permission can use it as much as they like; the host can revoke it live, mute, kick or ban. |
| Chat flooding, forged timestamps, markup | The host enforces everything: at most 5 messages in 5 s, an optional slow mode, 500 graphemes, control and bidi characters stripped, empty messages rejected. A claimed media time is honoured only within 5 s of the room position. The UI renders plain text only (React escaping); URLs are never linked or opened, only copied. | Offensive text: the host can delete messages, clear the chat, mute or ban. |
| Malformed file chunks or requests outside the manifest | Chunks are written in order into the per-room cache and the whole file is checked against the manifest's blake3 and size. File requests outside the manifest are protocol violations. | None known. |
| Lying status (ready, buffering) | It only affects the ready gate, which times out; the host can resume without that member. | See the gate timeout above. |

## Malicious host

| Threat | What we do | Residual risk |
|---|---|---|
| Hostile subtitle or audio files | Extension allowlists (subtitles: srt, ass, ssa, vtt, sub, sup; audio: a fixed list), size ceilings (20 MB subtitles, 1 GB audio, unless the member's room uses "No limit"), blake3 checked against the manifest, sanitized file names, a per-room cache deleted when the room ends, and text subtitles decoded to UTF-8. Members see name and size and can cancel. | Parser bugs in libass, FFmpeg or mpv. libmpv stays updatable (pinned build, `docs/libmpv-build.md`). |
| Unwanted magnets or torrents | Each member confirms a host's magnet (name and size shown) and can decline; the host sees "declined". | See Torrents. |
| Reading the chat | Chat is encrypted to the host, who relays it and can read all of it. The user guide says so. Chat is never written to disk or logs; members can export it explicitly. | Inherent to the host-star design. |
| Moderation abuse (mute, kick, ban) | Visible to the affected member; bans last only for the session. | Social, not technical. |
| Main media requests | The protocol has no message able to carry main-media bytes (enumeration test `no_media_over_datachannel_enumeration`), and only manifest files can be requested. | None known. |

## Your IP address

| Threat | What we do | Residual risk |
|---|---|---|
| Room peers learn your IP (WebRTC direct paths) | **Always use relay** (Settings, Network) forces TURN-only ICE, so peers only see the relay's address. | Without it, the host and members on direct paths see each other's addresses. The signaling server always sees your address. |
| The relay operator | Metered (or your own TURN) sees both ends' addresses and traffic volume, but only encrypted packets. | Inherent to TURN. |
| The BitTorrent swarm | Shown before the first torrent; see Torrents. | The relay setting does not hide it. |
| Latency in the members list | Each member's round-trip time to the host is shown next to their link (protocol v3, `MemberInfo.rtt_ms`). | A rough distance hint to other room members; no address. |

## Local data, logs and diagnostics

| Threat | What we do | Residual risk |
|---|---|---|
| Secrets in logs | Third-party crates are capped at `warn`; every log line goes through a scrubber that replaces registered secret values (API key, credentials, app name). A test logs the developer's real values at the most verbose level and greps for them. | A secret that was never registered (new code path). Code review in Milestone 9. |
| Full paths and chat in logs | The scrubber shortens every absolute Windows path to its last component (`...\name.mkv`), which also covers mpv and third-party messages. Chat content is never logged or put in diagnostics (test `chat_content_never_reaches_logs_or_diagnostics`). | File names stay in logs. |
| "Copy diagnostics" | The report removes credentials, keys, tokens, the Metered app name, IP addresses (except loopback) and full paths before it reaches the clipboard (`src/lib/redact.ts`, tests). | Room codes and file names are included (they help support). |
| Stored data | Only the user's own preferences, layout, window position, display name and the relay setup are stored (`config`). Nothing from a room is kept (test `persistence.test.ts`). Portable mode keeps everything in `<exe>\data`, including the WebView2 profile (`scripts/dev/portable-check.ps1`). The relay credentials are encrypted with Windows DPAPI for the current user (`credentials.dpapi`, `ice-last-good.dpapi`). | Malware running as the same user can decrypt the credentials. A portable copy moved to another PC or Windows user cannot decrypt them (DPAPI is per user). |
| Other local programs and the torrent stream server | See Torrents (127.0.0.1 only, random port, 128-bit token). | Same-user processes can read the same files anyway. |
| Telemetry | None. The app talks only to the signaling server, the TURN/STUN servers, Metered's credentials API (when set up), room peers, and the BitTorrent network in torrent rooms. | None. |
| Remote content in the UI | All fonts, icons and scripts are bundled; a strict CSP; a test fails on any remote resource. | None known. |

## Torrents (ADRs 0009, 0010)

| Threat | What we do | Residual risk |
|---|---|---|
| The swarm learns your IP address | Shown to every user before the first torrent ("You are responsible for what you stream... other people in its swarm can see your IP address"). The member confirms every magnet (name and size shown). | Inherent to BitTorrent. The relay setting does not hide it. |
| Staying in the swarm after the download | At 100% verified, all traffic stops. Peers disconnect, trackers get "stopped" (our own announce), the torrent is released, and the engine (DHT, listeners, sockets) shuts down when idle. A rejoin or restart plays the cached file and never re-adds the torrent. The UI says exactly: "Downloading and sharing have stopped. No further traffic." | **Other DHT nodes may keep our address listed for about 30 minutes** (BEP 5 cannot withdraw an announcement). Nobody can connect: every socket is closed and the DHT is shut down. |
| A member lies about its readiness (status `buffering` / `wait`, protocol v2) | It only affects the ready gate. The gate times out (15 s) and the host can "resume without X". An excused member cannot pause the room again until it was ready once. The values are display data (position, byte counts). | A member can still delay the room by up to the timeout, as before. |
| Hostile or malformed .torrent files (picked by the host, or offered as metadata) | Our own bencode reader runs before librqbit sees anything: a size cap (5 MB default, configurable), a depth limit (32), a node limit, a file limit (10,000), canonical integers, sorted unique keys, no trailing data, UTF-8 paths, path validation, a piece-count check, and at most 32 trackers (http, https and udp only) and 8 web seeds. Only the original info dictionary and the validated trackers are passed on. Tests feed truncated, nested, oversized, unsorted, non-UTF-8, traversal and reserved-name files. | A bug in our parser or in librqbit's own parser stays a residual risk; both run inside the app process. |
| Hostile torrent file names (path traversal, reserved Windows names, control characters, overlong paths) | Every path in the torrent is validated before anything is added: no `..`, no separators or drive letters inside components, no reserved names (CON, NUL, COM1...), no control characters, limited depth and length. One bad path refuses the whole torrent. | None known. |
| A huge or never-ending download | Cache size cap (Settings), a free-space check before starting, disk-full detection while downloading, a metadata timeout. | None. |
| The local streaming server is reachable by other programs | It listens on 127.0.0.1 only, on a random port, with a 128-bit token in the path (constant-time compare) and a Host header check. The token is redacted from logs. | Other local processes with the same user rights could read the media they could read from disk anyway. |
| Main media over the room's data channel | Never. Only manifest external files (`FileChunk`) and torrent METADATA (`TorrentMeta`) travel there. The latter is only for the room's announced infohash, chunked, capped at 5 MB by default (configurable, protocol ceiling 16 MiB), and only from the host. Protocol test `no_media_over_datachannel_enumeration`. | None. |
| A malicious host sends fake or hostile torrent metadata (ADR 0011) | The member parses it defensively: size cap, bencode depth and node limits, file and tracker caps, UTF-8 paths, path validation. The SHA-1 of the info dictionary must equal the announced infohash, and the host's chosen file must exist with the announced size. Otherwise it is refused and logged; nothing starts. Trackers in it are untrusted: only http, https and udp are kept, at most 32. A member never starts a torrent without confirming it. | A host can still pick a real but unwanted torrent: members see its name and size and may decline. |
| A member asks the host for other data | `TorrentMetaRequest` is answered only for the room's announced infohash; anything else is a protocol violation. Members cannot send `TorrentMeta`. | None. |
| Dependencies | rustls + ring only (no aws-lc, native-tls or OpenSSL in the Windows build; `scripts/check-tls-tree.ps1`, `deny.toml`). `cargo deny` and `cargo audit` run in CI. | RUSTSEC-2026-0293 (ringbuf, via librqbit-utp) is ignored with reasons: u8-only buffer, and uTP is off. `crypto-hash` is unmaintained (librqbit's SHA-1 on Windows uses the system CryptoAPI). |
