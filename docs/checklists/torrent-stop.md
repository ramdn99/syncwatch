# Manual check: traffic stops at 100% (Wireshark and TCPView)

The automated proof is `cargo test -p syncwatch-torrent` (test `stop_everything_at_100`) and
`scripts/dev/torrent-lab.ps1`. This checklist lets you confirm the same thing yourself with
Wireshark and TCPView. Use qBittorrent as the only seeder, and our own test torrent only.

## Prepare

1. Run the lab once and keep qBittorrent running:

   ```
   pwsh -NoProfile -File scripts/dev/torrent-lab.ps1 -KeepQbt
   ```

2. Note qBittorrent's listen port (Tools > Options > Connection) and the tracker port (9000).
3. Start TCPView. Filter on `syncwatch.exe` and `qbittorrent.exe`.
4. Start Wireshark on the **Adapter for loopback traffic capture** (Npcap loopback). Use this
   display filter, with your qBittorrent port:

   ```
   tcp.port == <qBittorrent port> || tcp.port == 9000 || udp.port == 9000
   ```

## While downloading

- [ ] TCPView shows `syncwatch.exe` connected to `127.0.0.1:<qBittorrent port>`.
- [ ] Wireshark shows BitTorrent traffic (`bittorrent` protocol) in both directions.
- [ ] One HTTP `GET /announce?...&event=started...` from SyncWatch to port 9000.

## At 100%

- [ ] The app shows "Downloading and sharing have stopped. No further traffic."
- [ ] Wireshark shows one HTTP `GET /announce?...&event=stopped...` from SyncWatch to port 9000.
      Nothing goes to that port afterwards.
- [ ] Within a second, TCPView shows every `syncwatch.exe` connection to qBittorrent closed. No
      new ones appear.
- [ ] TCPView shows no listening TCP port and no UDP endpoint of `syncwatch.exe` that belongs to
      the torrent engine. The loopback media server on `127.0.0.1` stays: it only feeds the
      local player.
- [ ] Wireshark shows no packet between the two programs for at least 60 s, while the video
      keeps playing.
- [ ] qBittorrent's peer list for the torrent is empty, and its "Uploaded" total does not
      change.

## Rejoin and restart

- [ ] Leave the room and join it again, or restart SyncWatch and join. The file plays at once.
- [ ] Wireshark shows no BitTorrent or tracker traffic, and TCPView shows no connection to
      qBittorrent.

## DHT (real network only)

With DHT on and a public torrent, other DHT nodes may keep our address listed for about 30
minutes after we stop (BEP 5 has no way to withdraw an announcement). Nobody can connect
during that time: every socket is closed and the DHT node is shut down. In Wireshark, filter
`udp && ip.src == <your address>`: after completion, SyncWatch sends no DHT (bencoded
`get_peers`/`announce_peer`) packets.
