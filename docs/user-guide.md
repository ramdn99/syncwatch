# SyncWatch user guide

Status: started in Milestone 7 with the torrent section. The full guide is written in
Milestone 9.

## Watching a torrent together

- **Host:**
  1. Open Change source, then Torrent magnet, and paste a magnet link.
  2. Choose **Get file list**. SyncWatch asks peers for the torrent's file list; this can take
     a moment and fails after 90 s if nobody has it.
  3. Pick the video file. Only that file is downloaded.
- **Members** see the torrent's name, the file the host chose and its size, and choose
  **Download and play** or **Decline**. Nothing is downloaded before they agree. Members never
  look for the file list themselves: they get it from the host. If the host is still getting it,
  they see "Waiting for the host to get the file list" and start as soon as it arrives.
- Everyone downloads their own copy and starts playing as soon as the beginning and the end of
  the file are there. Playback stays in sync while downloading.
- **Seeking to a part that is not downloaded yet** pauses the room and shows "Buffering n%"
  until enough has arrived. Then everyone continues together. Seeking back to downloaded parts
  is instant.
- **No peers:** SyncWatch shows "waiting for peers" and retries by itself. What is already
  downloaded keeps playing.

### When the download is complete

SyncWatch says: **"Downloading and sharing have stopped. No further traffic."**

At that moment it disconnects from every peer, tells the trackers it stopped, and shuts the
torrent engine down. That means no DHT, no listening port and no open connection. The video
keeps playing from the finished file, without a reload.

Other DHT nodes may keep your address listed for about 30 minutes; the DHT protocol has no way
to take a listing back. Nobody can connect to you during that time: every socket is closed and
the DHT is shut down.

When you join the room again, or restart SyncWatch, the finished file plays directly.
SyncWatch never joins the torrent's swarm again for it. There is no "keep seeding" option.

### Settings (Settings, then Torrents)

- Cache folder and size limit.
- Whether finished files are kept after leaving a room.
- Incoming port and bind address.
- DHT on or off.

**Your responsibility:** you are responsible for what you stream. Only use torrents you have
the right to download and share. While a torrent downloads, other people in its swarm can see
your IP address.
