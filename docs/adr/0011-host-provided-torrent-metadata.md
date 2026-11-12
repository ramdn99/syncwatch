# ADR 0011: Host-provided torrent metadata, members use the host's file

Status: accepted (fix after the Milestone 7 manual test). Date: 2026-11-12.

## Problem: members stuck at "getting file list"

In the manual test (two windows on one PC, a public magnet, DHT on) the host started
downloading at once, but the member stayed at "getting file list". The logs show:
- **Host:** its engine started at 13:30:15, had the metadata about 1 s later, and logged
  "torrent added (selected file only)" with 13 trackers.
- **Member:** its engine started at 13:30:18 with the same magnet. After that there was nothing:
  no metadata and no "torrent added". Its DHT bootstrap failed ("no successful lookups"), and it
  left 36 s later, still waiting.

### Root cause

The member fetched the torrent's metadata **from the swarm itself**, with librqbit's one-shot
magnet resolution:
- It uses the peers from the first tracker replies and the DHT.
- The tracker is asked again only after its interval, typically 30 minutes.

On one PC, or behind one NAT, that cannot work reliably:
- **The swarm refuses the second connection.** libtorrent-based peers (most of a public swarm,
  qBittorrent included) accept only one connection per IP by default. The peers the host was
  already connected to refused the member's handshake.
- **Reproduced in the lab** with qBittorrent keeping that default (`torrent-lab.ps1
  -SameIpDefault`). Whichever window connected first got the file list. The other window never
  did and never loaded.
- **The DHT gave nothing:** its bootstrap failed in the member's log.

The other suspects were checked and ruled out:
- the member UI never waits for a picker: it uses the descriptor's file index;
- the engine gets `only_files = [file_index]`;
- Accept does start the engine;
- engine status events do reach the UI.

There was a second gap: the waiting state said only "getting file list", with no hint of the
cause, until a 90 s timeout.

## Decision

- **The host is the only participant that gets the file list from the swarm** (or from a
  .torrent file). Its engine emits the sanitized metadata and its listen port as soon as the
  torrent is added.
- **New messages:**
  - `TorrentMeta { infohash, total, offset, host_port, data }`: host to member, chunked
    (16 KiB, bulk priority);
  - `TorrentMetaRequest { infohash }`: member to host.

  These carry torrent **metadata** only, never media:
  - they are bound to the room's announced infohash, and a request or offer for any other
    infohash is a protocol violation;
  - a member never sends `TorrentMeta`;
  - the protocol ceiling is 16 MiB; the app cap is 5 MB by default, configurable in
    Settings > Torrents;
  - the `no_media_over_datachannel` enumeration allows exactly `FileChunk` (manifest files) and
    `TorrentMeta`.
- **A member never asks the swarm for the file list.** Accepting without metadata shows
  "Waiting for the host to get the file list" and sends `TorrentMetaRequest`. When the offer
  arrives:
  1. the app verifies it with our defensive parser (`crates/torrent/src/meta.rs`): size cap,
     depth, node count, files, trackers, UTF-8 paths, path validation;
  2. the SHA-1 of the info dictionary must equal the announced infohash;
  3. the host's chosen file must exist in it, with the announced size;
  4. the member starts from those bytes (`start_with_meta`).

  A member that already has the metadata ignores further offers.
- **The host's engine is a peer for members on the same machine** (`127.0.0.1:<host_port>`).
  Because the swarm may refuse a second connection from the same address, the member downloads
  from the host instead (uploading while downloading has no cap).
- **The metadata lives in sync-core's `Local`**, which moves with every role change. So it
  reaches the participants this way:
  - late joiners and reconnects get it right after the Welcome;
  - after a host transfer or a succession, the new host keeps serving it.
- **Status text replaces the silent stall:**
  - "Waiting for the host to get the file list";
  - "Looking for peers (DHT on/off, N trackers)";
  - "N peers found";
  - on a timeout, a message naming the likely causes: nobody sharing, a firewall, DHT off.

  The host's progress block shows each member's phase, including "waiting for the host's file
  list".

## .torrent files (same change set, second commit)

- The host can pick a `.torrent` file instead of a magnet. It is read and validated locally with
  no network: name, infohash, total size and files show at once. Its sanitized bytes are what
  the host's engine adds and what members receive. Its trackers go into the room's magnet and
  into the announces.
- Changing source between magnet, file and local keeps the ADR 0010 rules. The old torrent
  stops completely, with "stopped" sent to its trackers. A finished file comes back from the
  cache with no swarm traffic.

## Web seeds and a public check

- librqbit 9.0.1 has no web-seed (BEP 19 `url-list`) support. Our parser keeps at most 8
  validated http/https web-seed URLs, only so that the count can drive the message "The torrent
  file was loaded, but no peers are sharing it yet. Its web seeds are not supported."
- **Internet Archive check (2026-11-12, about 60 s, trackers only, DHT off):**
  `SteamboatWillie_archive.torrent` (4.7 KB) parsed fine. It has 6 files, 2 of them video;
  2 trackers (bt1/bt2.archive.org); and 2 web seeds. Its trackers returned one peer, which
  delivered about 5.6 MB in 60 s at around 1.6 MB/s. So this torrent works through its peer,
  not through web seeds. IA torrents whose items have no BitTorrent peer at a given time would
  stall, showing the message above.

## Consequences

- **Members on other networks** download exactly as before, except that they never fetch the
  metadata.
- **The host does not appear in the swarm as a separate address for same-PC members.** They
  reach it directly over the loopback. Members on another machine in the same LAN still reach
  the host only through the swarm's addresses (no LAN peer hint is shared; that would expose
  LAN addresses).
- **Web seeds (BEP 19) are not supported by librqbit 9.0.1:** its source has no `url-list`
  handling. Torrents that live only on web seeds (typical for Internet Archive torrents) show
  "The torrent file was loaded, but no peers are sharing it yet. Its web seeds are not
  supported."
