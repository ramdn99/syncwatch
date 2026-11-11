# ADR 0010: Stop everything when the file is complete

Status: proposed (Milestone 7, before implementation). Date: 2026-11-06.
Source read: librqbit 9.0.1 (`session.rs` `pause`/`delete`/`stop`/`make_peer_rx`,
`torrent_state/live/mod.rs` `pause`, `torrent_state/mod.rs`), librqbit-tracker-comms 9.0.1
(`tracker_comms.rs`, `tracker_comms_udp.rs`, `tracker_comms_http.rs`) and librqbit-dht 9.0.1
(`dht.rs`).

## Spec (section 10, top rule)

When the selected file is 100% downloaded and hash-verified, all of the following happen for
that torrent:
- stop all downloading and uploading, and disconnect every peer;
- send the tracker "stopped" event;
- stop DHT and PEX announcing, and close its sockets;
- release the torrent and KEEP the file;
- if no other torrent is active, shut the engine down: zero torrent traffic and CPU while idle.

The swarm is never joined again (restart, rejoin, reload), and there is no "keep seeding" option.

## What librqbit does, step by step

| Step | librqbit 9.0.1 behaviour | Enough? |
|---|---|---|
| Know "complete and verified" | Every piece is SHA-1 checked before it counts as "have". `is_file_finished(file)` is true when all of the file's pieces are had | Yes |
| Stop downloading and uploading, disconnect peers | `Session::pause` cancels the torrent's cancellation token: every peer task ends and their TCP/uTP connections close | Yes |
| Stop PEX | PEX lives inside the peer connections, so it ends with them | Yes |
| Stop DHT announcing for this torrent | The peer stream is dropped on pause, and the DHT `RequestPeersStream` aborts its tasks on drop. No more `get_peers` or `announce_peer` | Yes |
| Remove our DHT announcement | **Impossible:** BEP 5 has no "unannounce". Nodes that stored us forget us when their entry expires (typically 30 min) | Closest: stop re-announcing (above) and stop the DHT node itself (below) |
| Tracker "stopped" | **Not sent.** The HTTP loop sends `started` once, then no event, and on pause the task is simply dropped. The UDP loop would send `stopped` only if an announce ran while paused, but it is dropped too. The request types are private, so we cannot reuse them | **Closest: our own final announce** (below) |
| Release the torrent, keep the file | `Session::delete(id, delete_files: false)` pauses, removes the torrent from the session and keeps the files. With persistence off, nothing about it is stored | Yes |
| Shut the engine down | `Session::stop()` pauses everything and cancels the session token, which stops the DHT, the TCP/uTP listeners, LSD and the UPnP tasks (all are children of it). It then sleeps 1 s for tasks to finish. Dropping the last `Arc<Session>` frees its sockets and the reqwest pool | Yes, with a check (below) |

## Decisions

1. **Completion detection.** The adapter watches the torrent's stats: the selected file
   finished, every piece hash-checked. Then it immediately runs the stop sequence below, with
   no grace period and no "seed to ratio".
2. **Stop sequence:**
   1. snapshot the stats (uploaded, downloaded), then `Session::pause`;
   2. **our own "stopped" announce** to every tracker of the torrent, in parallel, each limited to
      5 s: HTTP(S) GET `?event=stopped&left=0&...` through the same rustls client, and UDP per
      BEP 15 (connect, then announce with `event = 3`). The peer id is ours, because we set
      `SessionOptions::peer_id`. The tracker `key` cannot be the same (librqbit picks it at
      random internally). It is optional in BEP 3 and BEP 15, and trackers identify a peer by
      peer id and address. Failures are logged and ignored;
   3. `Session::delete(id, false)`, then delete the neighbour files that only hold boundary-piece
      bytes (ADR 0009);
   4. if no other torrent is active: `Session::stop().await`, then drop the session.
3. **Never rejoin.**
   - The room cache records finished torrents (info hash, file index, size, path).
   - A restart, rejoin or reload with a recorded, complete file never adds the torrent to an
     engine: the loopback server serves the file directly.
   - The persistence and fastresume options stay off: librqbit never restores a torrent by itself.
4. **Engine settings that keep traffic within the rule:**
   - UPnP port forwarding off (no router mappings left behind);
   - LSD (LAN multicast) off;
   - no session-wide default trackers (they would be added to every torrent);
   - DHT optional (on by default, a setting in the app), its persistence off;
   - incoming connections only on the configured port/bind.
5. **Handoff.**
   - While downloading, the loopback server reads through a `FileStream`.
   - At completion it switches to the finished file on disk under the same URL and token. An open
     range response keeps going from the same offset, so mpv needs no reload and no seek.

## Impossible claims and the closest alternative

- **"Stop DHT announcing" as removal from the DHT** cannot be done (BEP 5). We stop announcing at
  once and shut the DHT node down when idle. Our address stays in other nodes' stores until they
  expire it. The user guide says so, together with "the swarm sees your IP".
- **"Send the tracker stopped event"** is not done by librqbit. We send it ourselves (decision
  2.2), with our own key-less announce, which is the closest equivalent.
- **"Close its sockets" while another torrent is still active**: the DHT socket and the listeners
  belong to the session, so they stay open until the last torrent is done. The finished torrent's
  peer sockets close at once. Incoming connections for its info hash should be refused because
  it is no longer in the session; this is still to be verified, and the integration test checks
  it.

## Tests (named, required by section 12)

`stop_everything_at_100` (integration, in process, 127.0.0.1, DHT off) runs a librqbit seeder,
our leecher and a **test tracker** that records announces. At 100%:
- the seeder sees zero new bytes in either direction for 10 s;
- no connections remain (the seeder's peer list is empty);
- the tracker received `event=stopped` from our peer id, and no announce after it;
- our listen port can be bound again (the listener is closed);
- the file plays (blake3 matches the source);
- restarting the app and rejoining the room does not create an engine, and the seeder still
  sees zero bytes.

Engine shutdown is checked separately: no task of the session is left, and the process-wide
tokio task count goes back to its baseline.

**Manual checklist** (Wireshark and TCPView, `docs/checklists/torrent-stop.md`):
- with qBittorrent as the only seeder, TCPView shows our process's connections disappear at
  100%;
- Wireshark (filter: tracker and peer ports) shows the `stopped` announce and then nothing;
- after idle, no UDP DHT traffic from our process.

## Approval conditions (2026-11-11)

- **Stop at 100% stays the top rule.**
  - The completion message is exactly "Downloading and sharing have stopped. No further
    traffic.". It never claims that we left the swarm.
  - The user guide and the threat model say that other DHT nodes may keep our address listed
    for about 30 minutes, but nobody can connect: every socket is closed and the DHT is shut
    down.
- **Our "stopped" announce:**
  - it goes to every tracker of the torrent, in parallel, with a short timeout (3 s per tracker)
    on a background task, never blocking the UI or the room driver;
  - it is sent on completion, and also when the user leaves the room, the source is switched,
    or the user cancels mid-download;
  - it is tested against a local fake tracker (HTTP and UDP).
