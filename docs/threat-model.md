# SyncWatch threat model

Status: started in Milestone 7 (torrents). The full document (spec section 11) is completed
in Milestone 8.

## Torrents (ADRs 0009, 0010)

| Threat | What we do | Residual risk |
|---|---|---|
| The swarm learns your IP address | Shown to every user before the first torrent ("You are responsible for what you stream... other people in its swarm can see your IP address"). The member confirms every magnet (name and size shown). | Inherent to BitTorrent. The relay setting does not hide it. |
| Staying in the swarm after the download | At 100% verified, all traffic stops. Peers disconnect, trackers get "stopped" (our own announce), the torrent is released, and the engine (DHT, listeners, sockets) shuts down when idle. A rejoin or restart plays the cached file and never re-adds the torrent. The UI says exactly: "Downloading and sharing have stopped. No further traffic." | **Other DHT nodes may keep our address listed for about 30 minutes** (BEP 5 cannot withdraw an announcement). Nobody can connect: every socket is closed and the DHT is shut down. |
| Hostile torrent file names (path traversal, reserved Windows names, control characters, overlong paths) | Every path in the torrent is validated before anything is added: no `..`, no separators or drive letters inside components, no reserved names (CON, NUL, COM1...), no control characters, limited depth and length. One bad path refuses the whole torrent. | None known. |
| A huge or never-ending download | Cache size cap (Settings), a free-space check before starting, disk-full detection while downloading, a metadata timeout. | None. |
| The local streaming server is reachable by other programs | It listens on 127.0.0.1 only, on a random port, with a 128-bit token in the path (constant-time compare) and a Host header check. The token is redacted from logs. | Other local processes with the same user rights could read the media they could read from disk anyway. |
| Main media over the room's data channel | Never. Only manifest external files (`FileChunk`) and torrent METADATA (`TorrentMeta`) travel there. The latter is only for the room's announced infohash, chunked, capped at 5 MB by default (configurable, protocol ceiling 16 MiB), and only from the host. Protocol test `no_media_over_datachannel_enumeration`. | None. |
| A malicious host sends fake or hostile torrent metadata (ADR 0011) | The member parses it defensively: size cap, bencode depth and node limits, file and tracker caps, UTF-8 paths, path validation. The SHA-1 of the info dictionary must equal the announced infohash, and the host's chosen file must exist with the announced size. Otherwise it is refused and logged; nothing starts. Trackers in it are untrusted: only http, https and udp are kept, at most 32. A member never starts a torrent without confirming it. | A host can still pick a real but unwanted torrent: members see its name and size and may decline. |
| A member asks the host for other data | `TorrentMetaRequest` is answered only for the room's announced infohash; anything else is a protocol violation. Members cannot send `TorrentMeta`. | None. |
| Dependencies | rustls + ring only (no aws-lc, native-tls or OpenSSL in the Windows build; `scripts/check-tls-tree.ps1`, `deny.toml`). `cargo deny` and `cargo audit` run in CI. | RUSTSEC-2026-0293 (ringbuf, via librqbit-utp) is ignored with reasons: u8-only buffer, and uTP is off. `crypto-hash` is unmaintained (librqbit's SHA-1 on Windows uses the system CryptoAPI). |
